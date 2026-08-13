# shellcheck shell=bash
# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

# Tests for scripts/launcher.sh — the plugin's binary resolver. Sourced by
# tests/run.sh; uses $ROOT and the TEST_PASS / TEST_FAIL counters it resets
# per file.
#
# Every test runs the launcher against a THROWAWAY plugin root (its own pin)
# and a throwaway XDG_DATA_HOME / CC_LOADOUT_LINK_DIR, so no test can read or
# write the developer's real ~/.local/share/cc-loadout or ~/.local/bin. No
# test touches the network: Task 3 serves fixtures over file://.

# The launcher's own uname -> target mapping, duplicated so a test can name the
# asset the launcher will ask for. If these ever disagree, the download tests
# fail loudly rather than silently skipping.
launcher_target() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   echo x86_64-unknown-linux-musl ;;
    Linux-aarch64)  echo aarch64-unknown-linux-musl ;;
    Darwin-arm64)   echo aarch64-apple-darwin ;;
    Darwin-x86_64)  echo x86_64-apple-darwin ;;
    *) echo unsupported ;;
  esac
}

# A minimal plugin root: the real launcher plus a pin we control.
make_plugin_root() {
  local ver="$1" dir
  dir="$(mktemp -d)"
  mkdir -p "$dir/scripts" "$dir/.claude-plugin"
  cp "$ROOT/scripts/launcher.sh" "$dir/scripts/launcher.sh"
  chmod +x "$dir/scripts/launcher.sh"
  printf '%s\n' "$ver" > "$dir/.claude-plugin/cli-version"
  echo "$dir"
}

# Place an executable stand-in at the path the launcher will resolve, so
# resolution can be tested without any download.
place_fake_binary() {
  local data="$1" ver="$2"
  mkdir -p "$data/cc-loadout/bin/$ver"
  printf '#!/bin/sh\necho "cc-loadout %s"\necho "args:$*"\n' "$ver" \
    > "$data/cc-loadout/bin/$ver/cc-loadout"
  chmod +x "$data/cc-loadout/bin/$ver/cc-loadout"
}

# --- dev override wins over everything -------------------------------------
scratch="$(mktemp -d)"
printf '#!/bin/sh\necho "dev build ran: $*"\n' > "$scratch/devbin"
chmod +x "$scratch/devbin"
pr="$(make_plugin_root 9.9.9)"
# tests/run.sh sources this file under `set -euo pipefail`. A bare
# `var=$(...)` assignment whose command substitution fails aborts the
# sourcing script right there — silently, with no FAIL: line for the
# assertion below it, no later assertions in this file, and no Total:
# summary. Capturing the status explicitly with `&& rc=0 || rc=$?` keeps a
# failure local to this assertion instead of truncating the whole suite, so
# every bare `out=$(...)` in this file is guarded the same way even where the
# assertion itself only checks output, not rc.
out="$(CC_LOADOUT_BIN="$scratch/devbin" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" hook session-start 2>&1)" \
  && rc=0 || rc=$?
assert_contains "$out" "dev build ran: hook session-start" \
  "CC_LOADOUT_BIN short-circuits pin resolution"

out="$(CC_LOADOUT_BIN="$scratch/devbin" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>/dev/null)" \
  && rc=0 || rc=$?
assert_eq "$out" "$scratch/devbin" "--print-path honours CC_LOADOUT_BIN"
rm -rf "$scratch" "$pr"

# --- a malformed pin is rejected, and never becomes a path ------------------
# The pin is interpolated into a filesystem path, so traversal must be refused
# outright rather than sanitised.
for bad in "" "not-a-version" "1.2" "../../etc" "1/../../evil.0.0"; do
  scratch="$(mktemp -d)"
  pr="$(make_plugin_root "x")"
  printf '%s\n' "$bad" > "$pr/.claude-plugin/cli-version"
  out="$(XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
    sh "$pr/scripts/launcher.sh" --print-path 2>&1)" && rc=0 || rc=$?
  if [[ $rc -ne 0 && "$out" == *"bad version pin"* ]]; then
    TEST_PASS=$((TEST_PASS+1)); echo "  ok: pin '$bad' rejected"
  else
    TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: pin '$bad' not rejected (rc=$rc, out=$out)"
  fi
  rm -rf "$scratch" "$pr"
done

# --- an existing pinned binary is resolved and exec'd ----------------------
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
place_fake_binary "$scratch/data" 9.9.9
out="$(XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" --print-path 2>/dev/null)" \
  && rc=0 || rc=$?
assert_eq "$out" "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" \
  "--print-path resolves the pinned binary"
out="$(XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" hook session-end 2>&1)" \
  && rc=0 || rc=$?
assert_contains "$out" "args:hook session-end" "launcher execs with its arguments"
rm -rf "$scratch" "$pr"

# --- no-download mode refuses instead of reaching the network --------------
# This is session-end's mode: a session ending is not the moment to spend
# thirty seconds downloading.
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
out="$(CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>&1)" \
  && rc=0 || rc=$?
if [[ $rc -ne 0 && "$out" == *"downloads are disabled"* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: no-download mode refuses rather than downloading"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: no-download mode did not refuse (rc=$rc, out=$out)"
fi
rm -rf "$scratch" "$pr"

# --- a present-but-non-executable binary is treated as absent -------------
# Inherited intent from the retired hook.sh PATH lookup: an unusable binary
# must be indistinguishable from a missing one, never exec'd.
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
mkdir -p "$scratch/data/cc-loadout/bin/9.9.9"
printf '#!/bin/sh\necho nope\n' > "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout"
chmod 644 "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout"
out="$(CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>&1)" \
  && rc=0 || rc=$?
if [[ $rc -ne 0 && "$out" != *"nope"* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a non-executable pinned binary is not exec'd"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: non-executable pinned binary was used (rc=$rc, out=$out)"
fi
rm -rf "$scratch" "$pr"

# --- download fixtures -----------------------------------------------------
# Builds the exact asset pair the release publishes: the bare binary at the
# archive root, plus a `<bin>-<target>.sha256` whose body names the .tar.gz so
# `sha256sum -c` works from the download dir.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

make_release_fixture() {
  local dir="$1" ver="$2" target stage
  target="$(launcher_target)"
  mkdir -p "$dir/v$ver"
  stage="$(mktemp -d)"
  printf '#!/bin/sh\necho "cc-loadout %s"\necho "args:$*"\n' "$ver" > "$stage/cc-loadout"
  chmod +x "$stage/cc-loadout"
  ( cd "$stage" && tar -czf "$dir/v$ver/cc-loadout-$target.tar.gz" cc-loadout )
  ( cd "$dir/v$ver" && sha256_of "cc-loadout-$target.tar.gz" > "cc-loadout-$target.sha256" )
  rm -rf "$stage"
}

if [[ "$(launcher_target)" == unsupported ]]; then
  echo "  skip: download tests need a mapped target (this is $(uname -s)/$(uname -m))"
else

# --- a first run downloads, verifies and installs -------------------------
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
make_release_fixture "$scratch/rel" 9.9.9
out="$(CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --version 2>&1)" \
  && rc=0 || rc=$?
assert_contains "$out" "cc-loadout 9.9.9" "a first run downloads and execs the pinned binary"
if [[ -x "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: the download installed an executable at the pinned path"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: nothing executable at the pinned path after download"
fi
# The temp download dir must not survive a successful install.
leftovers="$(find "$scratch/data/cc-loadout/bin/9.9.9" -maxdepth 1 -name '.download.*' | wc -l | tr -d ' ')" \
  && rc=0 || rc=$?
assert_eq "$leftovers" "0" "no .download.* directory is left behind on success"
rm -rf "$scratch" "$pr"

# --- a corrupt tarball fails verification and installs nothing ------------
# The checksum is verified BEFORE anything from the archive is trusted, so a
# mismatch must leave no binary at all rather than a quarantined one.
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
make_release_fixture "$scratch/rel" 9.9.9
printf 'corrupted' >> "$scratch/rel/v9.9.9/cc-loadout-$(launcher_target).tar.gz"
out="$(CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>&1)" \
  && rc=0 || rc=$?
if [[ $rc -ne 0 && ! -e "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a checksum mismatch installs nothing"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: checksum mismatch was tolerated (rc=$rc, out=$out)"
fi
rm -rf "$scratch" "$pr"

# --- a missing asset explains the release window --------------------------
# Merging the release PR bumps the pin minutes before release.yml finishes
# uploading assets. Anyone installing in that window must be told to retry,
# not left staring at a curl error.
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
mkdir -p "$scratch/rel/v9.9.9"
out="$(CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>&1)" \
  && rc=0 || rc=$?
if [[ $rc -ne 0 && "$out" == *"retry shortly"* && "$out" == *"CC_LOADOUT_BIN"* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a missing asset names the retry and the escape hatch"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: missing-asset message unhelpful (rc=$rc, out=$out)"
fi
rm -rf "$scratch" "$pr"

fi  # end mapped-target guard

# --- PATH symlink reconciliation ------------------------------------------
# Four states, and the two "leave alone" rows are the important ones: a
# regular file is a standalone install and belongs to `doctor --fix`, and a
# symlink pointing somewhere else belongs to whoever made it.
resolve_link() { readlink "$1" 2>/dev/null || true; }

# absent -> created
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
place_fake_binary "$scratch/data" 9.9.9
XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" --print-path >/dev/null 2>&1
assert_eq "$(resolve_link "$scratch/bin/cc-loadout")" \
  "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" \
  "an absent link is created (and its dir with it)"
rm -rf "$scratch" "$pr"

# our own symlink at an older version -> repointed
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
place_fake_binary "$scratch/data" 9.9.9
mkdir -p "$scratch/bin"
ln -s "$scratch/data/cc-loadout/bin/8.8.8/cc-loadout" "$scratch/bin/cc-loadout"
XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" --print-path >/dev/null 2>&1
assert_eq "$(resolve_link "$scratch/bin/cc-loadout")" \
  "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" \
  "our own symlink is repointed at the pin (even when dangling)"
rm -rf "$scratch" "$pr"

# a regular file -> untouched
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
place_fake_binary "$scratch/data" 9.9.9
mkdir -p "$scratch/bin"
printf '#!/bin/sh\necho standalone\n' > "$scratch/bin/cc-loadout"
chmod +x "$scratch/bin/cc-loadout"
XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" --print-path >/dev/null 2>&1
if [[ -f "$scratch/bin/cc-loadout" && ! -L "$scratch/bin/cc-loadout" \
      && "$(cat "$scratch/bin/cc-loadout")" == *standalone* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a standalone regular file is left untouched"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: the launcher clobbered a standalone install"
fi
rm -rf "$scratch" "$pr"

# a foreign symlink -> untouched
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
place_fake_binary "$scratch/data" 9.9.9
mkdir -p "$scratch/bin" "$scratch/elsewhere"
printf '#!/bin/sh\necho elsewhere\n' > "$scratch/elsewhere/cc-loadout"
chmod +x "$scratch/elsewhere/cc-loadout"
ln -s "$scratch/elsewhere/cc-loadout" "$scratch/bin/cc-loadout"
XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" --print-path >/dev/null 2>&1
assert_eq "$(resolve_link "$scratch/bin/cc-loadout")" "$scratch/elsewhere/cc-loadout" \
  "a symlink pointing outside our data dir is left untouched"
rm -rf "$scratch" "$pr"

# --- version GC -----------------------------------------------------------
# Unpacked binaries are several MiB each; twenty releases would otherwise leave
# a pile behind. Only strict N.N.N siblings may be swept.
if [[ "$(launcher_target)" != unsupported ]]; then
scratch="$(mktemp -d)"
pr="$(make_plugin_root 9.9.9)"
make_release_fixture "$scratch/rel" 9.9.9
place_fake_binary "$scratch/data" 8.8.8
mkdir -p "$scratch/data/cc-loadout/bin/notaversion" "$scratch/data/cc-loadout/bin/9.9.9-rc1"
CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path >/dev/null 2>&1
if [[ ! -e "$scratch/data/cc-loadout/bin/8.8.8" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: GC removed the superseded 8.8.8 dir"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: GC left the superseded 8.8.8 dir behind"
fi
if [[ -d "$scratch/data/cc-loadout/bin/notaversion" && -d "$scratch/data/cc-loadout/bin/9.9.9-rc1" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: GC left non-N.N.N directories alone"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: GC deleted a directory it does not own"
fi
if [[ -x "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: GC kept the pinned version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: GC removed the pinned version"
fi
rm -rf "$scratch" "$pr"
fi
