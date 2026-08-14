#!/bin/sh
# cc-loadout plugin launcher — resolves the version-pinned CLI binary.
#
# Runs the release binary pinned in `.claude-plugin/cli-version`, downloading it
# into the plugin data dir on first use, so the binary THIS SCRIPT resolves and
# execs is always the exact pinned build. That guarantee is scoped to this
# resolution only: the bundled skills invoke `cc-loadout` by name, through
# PATH, so if a regular file ever sits at that PATH location instead of our
# symlink (see reconcile_link below), the skills drive THAT build, not this
# one — hooks/hook.sh's standalone-install hint exists for exactly that case,
# and `doctor --fix` converges it. Two calling conventions:
#
#   launcher.sh --print-path        resolve, print the path, exit
#   launcher.sh <args...>           resolve, then exec the binary with <args>
#
# Every message goes to stderr. `--print-path` writes ONLY the path to stdout,
# because its caller is a SessionStart hook whose stdout is injected into the
# session's context.
set -eu

# Fetches the pinned release, verifies its checksum before anything from the
# archive is trusted, and installs the binary atomically.
download() {
  # Named in every hint below that tells someone to build from source: the
  # retired curl|bash installer gave a runnable command, and a bare "set
  # CC_LOADOUT_BIN" without it does not.
  repo="XBlueSky/cc-loadout"

  case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64) target=aarch64-unknown-linux-musl ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  *)
    echo "cc-loadout launcher: unsupported platform $(uname -s)/$(uname -m);" \
      "build from source instead: git clone https://github.com/$repo &&" \
      "cd cc-loadout && ./install.sh — then set CC_LOADOUT_BIN to the result" >&2
    exit 1
    ;;
  esac
  command -v curl >/dev/null 2>&1 || {
    echo "cc-loadout launcher: curl is required to download the pinned binary" >&2
    exit 1
  }

  asset="cc-loadout-$target.tar.gz"
  # taiki-e/upload-rust-binary-action names the checksum `<bin>-<target>.sha256`
  # (no .tar.gz); its content references the .tar.gz filename, so `-c` works
  # from the download dir. release.yml reproduces that layout by hand.
  #
  # The checksum guards download integrity, not release authenticity — it ships
  # next to the tarball. The trust root is the version pin plus HTTPS.
  sum="cc-loadout-$target.sha256"

  # HTTPS is enforced for the real release host and relaxed only when a base is
  # explicitly overridden, which the test suite does with file:// fixtures.
  # Keying on the override (not on the URL scheme) keeps the default path
  # incapable of being downgraded by anything a redirect could say.
  if [ -n "${CC_LOADOUT_RELEASE_BASE:-}" ]; then
    base="$CC_LOADOUT_RELEASE_BASE/v$VER"
    proto_args=""
  else
    base="https://github.com/$repo/releases/download/v$VER"
    proto_args="--proto =https --proto-redir =https"
  fi

  mkdir -p "$DATA/bin/$VER"
  # tmp dir inside the version dir: same filesystem, so the final mv is atomic
  # even when two sessions race the first download.
  tmp=$(mktemp -d "$DATA/bin/$VER/.download.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT

  echo "cc-loadout launcher: downloading $asset (v$VER)" >&2
  # Bounded on purpose: the SessionStart hook that calls this has a 90s harness
  # timeout, and being SIGKILLed there would skip the trap above and orphan
  # $tmp. Failing on our own terms keeps the cleanup reachable.
  #
  # The `curl ...; rc=$? ; if [ "$rc" ...` shape (rather than a bare
  # `if ! curl ...`) is deliberate: under `set -eu` a failing command that
  # is not itself the direct subject of `if`/`!`/`&&`/`||` aborts the script
  # right there, before this line ever gets to branch on curl's actual exit
  # status — the exact thing the branch below needs to do.
  # shellcheck disable=SC2086  # $proto_args must word-split
  curl -fsSL $proto_args --connect-timeout 10 --max-time 30 \
    -o "$tmp/$asset" "$base/$asset" >&2 && curl_rc=0 || curl_rc=$?
  if [ "$curl_rc" -ne 0 ]; then
    case "$curl_rc" in
    22 | 37)
      # 22: curl -f turns any HTTP >=400 into this — the real shape a missing
      # release asset takes against the actual HTTPS release host, and the
      # one case that IS the release window: release-plz bumps the pin
      # minutes before release.yml finishes uploading assets, so the tag
      # exists but this asset does not yet.
      # 37: file:// scheme's own "couldn't open file", which is what the SAME
      # missing-asset situation looks like against the test suite's file://
      # fixtures (CC_LOADOUT_RELEASE_BASE) instead of a real HTTP 404 — grouped
      # here so the tests exercise the identical wording production hits,
      # without this code ever being reachable through a real https:// base.
      echo "cc-loadout launcher: no asset for $target in release v$VER." \
        "If v$VER was just published, retry shortly; otherwise build from" \
        "source instead: git clone https://github.com/$repo && cd cc-loadout" \
        "&& ./install.sh — then set CC_LOADOUT_BIN to the result" >&2
      ;;
    *)
      # Anything else — DNS failure, no network, a proxy, our own
      # --max-time, ENOSPC — is a transport failure, not a missing release
      # asset, and telling someone to "retry shortly" for a broken network
      # actively misdiagnoses it. Name curl's own exit code (`man curl`
      # lists them) so it stays diagnosable without this script trying to
      # enumerate every one.
      echo "cc-loadout launcher: could not reach $base/$asset (curl exit" \
        "$curl_rc); check your network and retry" >&2
      ;;
    esac
    exit 1
  fi
  # shellcheck disable=SC2086  # $proto_args must word-split
  curl -fsSL $proto_args --connect-timeout 10 --max-time 30 \
    -o "$tmp/$sum" "$base/$sum" >&2 && curl_rc=0 || curl_rc=$?
  if [ "$curl_rc" -ne 0 ]; then
    echo "cc-loadout launcher: could not fetch $sum for v$VER (curl exit" \
      "$curl_rc); refusing to use an unverified download" >&2
    exit 1
  fi

  # Verify BEFORE anything from the archive is trusted or executed. This is
  # the one security-relevant failure path, so — unlike the fail-soft PATH
  # bookkeeping elsewhere in this script — it gets its own explicit,
  # cc-loadout-prefixed message rather than relying on sha256sum's/shasum's
  # own "FAILED" line (still shown above it) to speak for itself.
  if command -v sha256sum >/dev/null 2>&1; then
    if ! (cd "$tmp" && sha256sum -c "$sum") >&2; then
      echo "cc-loadout launcher: checksum verification FAILED for $asset —" \
        "nothing was installed" >&2
      exit 1
    fi
  elif command -v shasum >/dev/null 2>&1; then
    if ! (cd "$tmp" && shasum -a 256 -c "$sum") >&2; then
      echo "cc-loadout launcher: checksum verification FAILED for $asset —" \
        "nothing was installed" >&2
      exit 1
    fi
  else
    echo "cc-loadout launcher: neither sha256sum nor shasum found —" \
      "cannot verify the download" >&2
    exit 1
  fi

  tar -xzf "$tmp/$asset" -C "$tmp" cc-loadout
  chmod +x "$tmp/cc-loadout"
  mv -f "$tmp/cc-loadout" "$BIN"
  rm -rf "$tmp"
  trap - EXIT
}
# Point $LINK at $BIN so the interactive TUI is reachable by name, but only
# where doing so cannot destroy something we did not create. Idempotent: it
# runs on every invocation, including when the binary was already present.
#
# The one refused state — a regular file — is a standalone install from
# install.sh or `cargo`. Moving a user's file without consent is `doctor
# --fix`'s job, invoked deliberately; see hooks/hook.sh for the hint that
# gets them there.
#
# Every write below is best-effort and every refusal is non-fatal: this is a
# PATH convenience, not something a session should fail to start over. But a
# review found that silence had a cost of its own — a broken PATH entry with
# nothing anywhere saying why — so the three cases below that can leave
# 'cc-loadout' unusable on PATH each get exactly one diagnostic line now.
reconcile_link() {
  if [ -L "$LINK" ]; then
    cur=$(readlink "$LINK" 2>/dev/null || true)
    case "$cur" in
    "$DATA"/bin/*/cc-loadout)
      # Ours, so a stale version (or a dangling link left by GC) is safe to
      # repoint. -n matters: without it, ln follows an existing symlink to a
      # directory and creates the link inside it.
      if [ "$cur" != "$BIN" ] && ! ln -sfn "$BIN" "$LINK" 2>/dev/null; then
        # reconcile_link now runs BEFORE gc_old_versions (see the call site
        # below for why), so $cur's directory still exists at this exact
        # point — but gc_old_versions is about to reclaim it unconditionally,
        # since its name no longer matches the pin. Once that happens this
        # link goes from "stale but working" to "dangling", and nothing else
        # would ever say why 'cc-loadout' on PATH stopped running.
        echo "cc-loadout launcher: could not repoint $LINK from $cur to" \
          "$BIN ($LINK_DIR may need to be writable) — it will keep running" \
          "the old binary until this is fixed, and stop working entirely" \
          "once that version is garbage-collected" >&2
      fi
      ;;
    *)
      # Someone else's link — not ours to move. But if it is also DANGLING
      # (its target does not exist), hooks/hook.sh's standalone-install check
      # never fires either: `[ -L "$link" ]` is true there, so it takes the
      # "it's a symlink, skip" branch and never reaches the regular-file
      # check that would otherwise name a broken install. A foreign symlink
      # with a live target still runs something when invoked by name; a
      # dangling one runs nothing and explains nothing. Name it here, once,
      # since nothing else ever will.
      if [ ! -e "$LINK" ]; then
        echo "cc-loadout launcher: $LINK is a broken symlink to $cur;" \
          "the pinned binary is at $BIN but 'cc-loadout' on PATH will not" \
          "run it until $LINK is fixed or removed" >&2
      fi
      ;;
    esac
    return 0
  fi
  # -e is false for a broken symlink, but -L above already caught every
  # symlink, so anything left here is a real entry: file, dir or device.
  if [ -e "$LINK" ]; then
    return 0
  fi
  if ! mkdir -p "$LINK_DIR" 2>/dev/null; then
    echo "cc-loadout launcher: the pinned binary is at $BIN but could not" \
      "be exposed at $LINK ($LINK_DIR could not be created — it may need" \
      "to be writable)" >&2
    return 0
  fi
  if ! ln -sfn "$BIN" "$LINK" 2>/dev/null; then
    echo "cc-loadout launcher: the pinned binary is at $BIN but could not" \
      "be exposed at $LINK (creating the symlink failed — $LINK_DIR may" \
      "need to be writable)" >&2
  fi
}

# Keep only the pinned version. Unpacked binaries are several MiB each, so
# without this every release leaves another copy behind forever. Runs on
# every invocation (not just after a fresh download) — see the call site
# below — so it stays cheap on purpose: a `for` over what is normally one or
# two directory entries.
#
# Removing a binary a concurrent session is still executing is safe on Unix —
# the inode survives for the running process. A plugin downgrade re-downloads,
# which is an acceptable price for a bounded directory.
gc_old_versions() {
  for d in "$DATA"/bin/*; do
    [ -d "$d" ] || continue
    name=${d##*/}
    [ "$name" != "$VER" ] || continue
    # Same two-step test as the pin validator, for the same reason: only
    # sweep names we are certain this script created. `9.9.9-rc1`,
    # `notaversion` and a stray `.download.XXXXXX` are all left alone.
    case "$name" in
    *[!0-9.]*) continue ;;
    [0-9]*.[0-9]*.[0-9]*)
      # Fail-soft, matching every other fallible op in reconcile_link: pruning
      # is best-effort housekeeping, and under `set -eu` a bare failing rm
      # here (read-only data dir, permission error, restrictive mount) would
      # abort the whole launcher before reconcile_link runs and the binary
      # execs. A stray leftover directory is an acceptable price; a session
      # that fails to start is not.
      rm -rf "$d" 2>/dev/null || true
      ;;
    esac
  done
}

print_path_only=0
if [ "${1:-}" = "--print-path" ]; then
  print_path_only=1
  shift
fi

# Dev override: run a local build, skip pinning entirely.
if [ -n "${CC_LOADOUT_BIN:-}" ]; then
  if [ "$print_path_only" -eq 1 ]; then
    printf '%s\n' "$CC_LOADOUT_BIN"
    exit 0
  fi
  exec "$CC_LOADOUT_BIN" "$@"
fi

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PIN_FILE="$ROOT/.claude-plugin/cli-version"
VER=$(tr -d '[:space:]' <"$PIN_FILE" 2>/dev/null || true)

# Strict enough to be safe, not a semver parser. $VER is interpolated into a
# filesystem path below, so the second pattern is load-bearing: the first alone
# accepts `1/../../evil.0.0` (glob `*` spans slashes), which would resolve
# outside the data dir. Rejecting every character that is not a digit or a dot
# closes that without needing to enumerate traversal shapes.
pin_ok=0
case "$VER" in
[0-9]*.[0-9]*.[0-9]*)
  case "$VER" in
  *[!0-9.]*) ;;
  *) pin_ok=1 ;;
  esac
  ;;
esac
if [ "$pin_ok" -ne 1 ]; then
  echo "cc-loadout launcher: bad version pin '$VER' in .claude-plugin/cli-version" >&2
  exit 1
fi

# Deliberately NOT $CLAUDE_PLUGIN_DATA, which cc-uplink's launcher prefers.
# Claude Code sets that for MCP servers; whether a hook process receives it is
# not established, and cc-loadout has no MCP server so nothing else will ever
# set it. If it were present in one context and absent in another, the same
# machine would resolve two data dirs, download twice, and flap the PATH
# symlink between them. One deterministic path is strictly better here.
#
# XDG_DATA_HOME and CC_LOADOUT_LINK_DIR are normalized before they are
# concatenated into a path below, for two reasons — fixed identically in
# install.sh (its own copy of normalize_dir_var), and on the Rust side in
# src/main.rs's resolve_env() and src/doctor.rs's link_path(). Touch one,
# touch all four, or they will re-diverge exactly the way this fix-wave item
# found them:
#
#  1. A trailing slash. This is a naive string join, so "/x/" would
#     concatenate here as "/x//cc-loadout" while Rust's PathBuf::join
#     silently suppresses the doubled separator and gets "/x/cc-loadout" from
#     the SAME input. Two spellings of one file break every exact-string
#     comparison built on them — most importantly reconcile_link's
#     `"$DATA"/bin/*/cc-loadout` pattern below, which would then never
#     recognize a symlink `doctor --fix` (Rust-spelled) wrote as "ours",
#     classify it foreign, and leave it unrepointed forever.
#  2. A relative value. The XDG Base Directory spec says a relative path in
#     these variables is invalid and must be ignored — otherwise it resolves
#     against whatever this process's cwd happens to be, and this script,
#     doctor, and install.sh each run with a different one.
#
# Prints the normalized value, or nothing if it should be treated as unset
# (relative, empty, or nothing but slashes).
normalize_dir_var() {
  v="$1"
  case "$v" in
  /*) ;;
  *) v="" ;;
  esac
  while [ -n "$v" ] && [ "${v%/}" != "$v" ]; do v="${v%/}"; done
  printf '%s' "$v"
}

xdg_data_home="$(normalize_dir_var "${XDG_DATA_HOME:-}")"
link_dir_env="$(normalize_dir_var "${CC_LOADOUT_LINK_DIR:-}")"

DATA="${xdg_data_home:-$HOME/.local/share}/cc-loadout"
BIN="$DATA/bin/$VER/cc-loadout"
LINK_DIR="${link_dir_env:-$HOME/.local/bin}"
LINK="$LINK_DIR/cc-loadout"

# `-x` and not `-f`: a directory would also pass a bare `-x` (its traversal bit
# is set by default), and a present-but-non-executable file must be treated as
# absent rather than exec'd — an unusable binary is indistinguishable from a
# missing one.
if [ ! -f "$BIN" ] || [ ! -x "$BIN" ]; then
  if [ -n "${CC_LOADOUT_LAUNCHER_NO_DOWNLOAD:-}" ]; then
    echo "cc-loadout launcher: $BIN is not available and downloads are disabled" >&2
    exit 1
  fi
  download
fi

# reconcile_link MUST run before gc_old_versions: on a pin bump, the only
# thing still keeping an old version's directory around is the very link
# reconcile_link is about to repoint. Reversed, an unwritable $LINK_DIR would
# hit this sequence: gc deletes the old version directory the existing,
# WORKING link still points at, then reconcile_link's own repoint fails (see
# its own diagnostic above) — the launcher would have just broken a
# 'cc-loadout' on PATH that worked a moment earlier, for a reason unrelated to
# the pin bump itself. Doing it in this order means a successful repoint
# always lands before its old target can be reclaimed, and a failed one still
# leaves the old (stale, but working) target alive until gc_old_versions
# concludes it is no longer the pin.
reconcile_link
gc_old_versions

if [ "$print_path_only" -eq 1 ]; then
  printf '%s\n' "$BIN"
  exit 0
fi
exec "$BIN" "$@"
