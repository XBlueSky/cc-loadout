#!/bin/sh
# cc-loadout plugin launcher — resolves the version-pinned CLI binary.
#
# Runs the release binary pinned in `.claude-plugin/cli-version`, downloading it
# into the plugin data dir on first use so the plugin's skills and the binary
# they drive can never version-skew. Two calling conventions:
#
#   launcher.sh --print-path        resolve, print the path, exit
#   launcher.sh <args...>           resolve, then exec the binary with <args>
#
# Every message goes to stderr. `--print-path` writes ONLY the path to stdout,
# because its caller is a SessionStart hook whose stdout is injected into the
# session's context.
set -eu

# Filled in by later tasks; defined here so the resolution path below is
# complete and testable on its own.
download() {
  case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64) target=aarch64-unknown-linux-musl ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  *)
    echo "cc-loadout launcher: unsupported platform $(uname -s)/$(uname -m);" \
      "build from source and set CC_LOADOUT_BIN" >&2
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
    base="https://github.com/XBlueSky/cc-loadout/releases/download/v$VER"
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
  # shellcheck disable=SC2086  # $proto_args must word-split
  if ! curl -fsSL $proto_args --connect-timeout 10 --max-time 30 \
    -o "$tmp/$asset" "$base/$asset" >&2; then
    echo "cc-loadout launcher: no asset for $target in release v$VER." \
      "If v$VER was just published, retry shortly; otherwise build from source" \
      "and set CC_LOADOUT_BIN" >&2
    exit 1
  fi
  # shellcheck disable=SC2086  # $proto_args must word-split
  if ! curl -fsSL $proto_args --connect-timeout 10 --max-time 30 \
    -o "$tmp/$sum" "$base/$sum" >&2; then
    echo "cc-loadout launcher: could not fetch $sum for v$VER; refusing to use" \
      "an unverified download" >&2
    exit 1
  fi

  # Verify BEFORE anything from the archive is trusted or executed.
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "$sum") >&2
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "$sum") >&2
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
reconcile_link() {
  if [ -L "$LINK" ]; then
    cur=$(readlink "$LINK" 2>/dev/null || true)
    case "$cur" in
    "$DATA"/bin/*/cc-loadout)
      # Ours, so a stale version (or a dangling link left by GC) is safe to
      # repoint. -n matters: without it, ln follows an existing symlink to a
      # directory and creates the link inside it.
      [ "$cur" = "$BIN" ] || ln -sfn "$BIN" "$LINK" 2>/dev/null || true
      ;;
    *) : ;; # someone else's link — not ours to move
    esac
    return 0
  fi
  # -e is false for a broken symlink, but -L above already caught every
  # symlink, so anything left here is a real entry: file, dir or device.
  if [ -e "$LINK" ]; then
    return 0
  fi
  mkdir -p "$LINK_DIR" 2>/dev/null || return 0
  ln -sfn "$BIN" "$LINK" 2>/dev/null || true
}

# Keep only the pinned version. Unpacked binaries are several MiB each, so
# without this every release leaves another copy behind forever.
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
    [0-9]*.[0-9]*.[0-9]*) rm -rf "$d" ;;
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
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/cc-loadout"
BIN="$DATA/bin/$VER/cc-loadout"
LINK_DIR="${CC_LOADOUT_LINK_DIR:-$HOME/.local/bin}"
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
  gc_old_versions
fi

reconcile_link

if [ "$print_path_only" -eq 1 ]; then
  printf '%s\n' "$BIN"
  exit 0
fi
exec "$BIN" "$@"
