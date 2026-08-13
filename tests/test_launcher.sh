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
out="$(CC_LOADOUT_BIN="$scratch/devbin" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" hook session-start 2>&1)"
assert_contains "$out" "dev build ran: hook session-start" \
  "CC_LOADOUT_BIN short-circuits pin resolution"

out="$(CC_LOADOUT_BIN="$scratch/devbin" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_LINK_DIR="$scratch/bin" sh "$pr/scripts/launcher.sh" --print-path 2>/dev/null)"
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
  sh "$pr/scripts/launcher.sh" --print-path 2>/dev/null)"
assert_eq "$out" "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" \
  "--print-path resolves the pinned binary"
out="$(XDG_DATA_HOME="$scratch/data" CC_LOADOUT_LINK_DIR="$scratch/bin" \
  sh "$pr/scripts/launcher.sh" hook session-end 2>&1)"
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
