# shellcheck shell=bash
# Validates the Claude Code plugin manifests. Sourced by tests/run.sh; uses
# $ROOT and the TEST_PASS / TEST_FAIL counters it resets per file.

plp="$ROOT/.claude-plugin/plugin.json"
mfp="$ROOT/.claude-plugin/marketplace.json"

if jq -e . "$plp" >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: plugin.json is valid JSON"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: plugin.json invalid or missing"
fi

if jq -e . "$mfp" >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: marketplace.json is valid JSON"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: marketplace.json invalid or missing"
fi

if [[ "$(jq -r '.name' "$plp" 2>/dev/null)" == "cc-loadout" \
      && "$(jq -r '.version' "$plp" 2>/dev/null)" != "null" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: plugin.json has name + version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: plugin.json missing name or version"
fi

# GOTCHA 1: marketplace.json must NOT set a plugin version (plugin.json wins silently)
if [[ "$(jq -r '.plugins[0].version // "unset"' "$mfp" 2>/dev/null)" == "unset" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: marketplace.json does not duplicate version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: marketplace.json sets a plugin version (silent conflict with plugin.json)"
fi

# GOTCHA 2: the bundled plugin's source must be the repo root
if [[ "$(jq -r '.plugins[0].source' "$mfp" 2>/dev/null)" == "./" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: marketplace plugin source is ./"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: marketplace plugin source is not ./"
fi

# The bundled skill must actually exist, or the installed plugin exposes nothing.
if [[ -f "$ROOT/skills/init/SKILL.md" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: bundled skill skills/init/SKILL.md present"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: skills/init/SKILL.md missing (plugin would expose no skill)"
fi

# plugin.json version must track the crate version (prevent drift)
cargo_ver="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
plugin_ver="$(jq -r '.version' "$plp" 2>/dev/null)"
if [[ -n "$cargo_ver" && "$cargo_ver" == "$plugin_ver" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: plugin.json version matches Cargo.toml ($cargo_ver)"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: plugin.json version ($plugin_ver) != Cargo.toml ($cargo_ver)"
fi

# The plugin now owns its hooks; the manifest and the shim must exist.
hkp="$ROOT/hooks/hooks.json"
if jq -e . "$hkp" >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hooks/hooks.json is valid JSON"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hooks/hooks.json invalid or missing"
fi

# Each event must point at the shared shim, plugin-root relative, and pass its
# own event name through as the argument the binary expects.
for ev in SessionStart:session-start SessionEnd:session-end; do
  key="${ev%%:*}"; arg="${ev##*:}"
  c="$(jq -r --arg e "$key" '.hooks[$e][0].hooks[0].command // ""' "$hkp" 2>/dev/null)"
  if [[ "$c" == *'${CLAUDE_PLUGIN_ROOT}/hooks/hook.sh '* && "$c" == *" $arg" ]]; then
    TEST_PASS=$((TEST_PASS+1)); echo "  ok: $key invokes hook.sh with '$arg'"
  else
    TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: $key command wrong: $c"
  fi
done

if [[ -f "$ROOT/hooks/hook.sh" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hooks/hook.sh present"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hooks/hook.sh missing (hooks.json would point at nothing)"
fi

# Regression guard: no shim may reference the deleted lib/ scripts.
if grep -rq "lib/session-.*-hook.sh" "$ROOT/hooks/" 2>/dev/null; then
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: a shim still references the retired lib/ hook scripts"
else
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: no shim references the retired lib/ scripts"
fi

# SessionStart must be able to absorb a first-run download. The launcher bounds
# its own curls well inside this, so the harness timeout is a backstop, not the
# thing being relied on.
if [[ "$(jq -r '.hooks.SessionStart[0].hooks[0].timeout' "$hkp" 2>/dev/null)" == "90" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: SessionStart timeout leaves room for a first download"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: SessionStart timeout is not 90"
fi

# --- hook.sh contract -------------------------------------------------------
# stdout is injected into the session's context, so what the shim prints there
# is user-facing and every path must still exit 0.
#
# tests/run.sh sources this file under `set -euo pipefail`. A bare
# `hook_out="$(...)"` followed by a separate `hook_exit=$?` line is not enough
# to guard that: bash *does* propagate the inner command's status onto the
# assignment, so under `set -e` a non-zero status there aborts the script
# right there — the `hook_exit=$?` line is never reached, no FAIL: line is
# printed for it, and every assertion after it in this file silently vanishes
# along with the Total: summary. hook.sh exits 0 on every path by design, so
# that abort never fires today and the hazard is invisible; it must not be
# reintroduced the moment that invariant is ever violated by mistake. Guarding
# every site with `&& hook_exit=0 || hook_exit=$?` (the same convention
# tests/test_launcher.sh uses for the launcher) keeps a failure local to its
# own assertion and makes hook_exit an assertion of fact rather than a value
# that happens to read 0 either way.
hook_scratch="$(mktemp -d)"
mkdir -p "$hook_scratch/bin" "$hook_scratch/link"
printf '#!/bin/sh\necho "ran:$*"\n' > "$hook_scratch/bin/stub"
chmod +x "$hook_scratch/bin/stub"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  CC_LOADOUT_BIN="$hook_scratch/bin/stub" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  bash "$ROOT/hooks/hook.sh" session-start 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ "$hook_out" == *"ran:hook session-start"* && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hook.sh runs the resolved binary with its event"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh did not run the resolved binary (exit=$hook_exit, out=$hook_out)"
fi

# A binary that fails must not break the session and must not narrate to stdout.
printf '#!/bin/sh\necho "boom" >&2\nexit 2\n' > "$hook_scratch/bin/stub"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  CC_LOADOUT_BIN="$hook_scratch/bin/stub" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  bash "$ROOT/hooks/hook.sh" session-start 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ -z "$hook_out" && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hook.sh exits 0 and stays off stdout when the binary fails"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh misbehaved for a failing binary (exit=$hook_exit, out=$hook_out)"
fi
rm -rf "$hook_scratch"

# Unresolvable at session-start: say so once, on stdout, and still exit 0.
hook_scratch="$(mktemp -d)"
mkdir -p "$hook_scratch/link"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  XDG_DATA_HOME="$hook_scratch/data" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 \
  bash "$ROOT/hooks/hook.sh" session-start 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ "$hook_out" == *"CC_LOADOUT_BIN"* && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: an unresolvable CLI prints a hint at session-start"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: no session-start hint for an unresolvable CLI (exit=$hook_exit, out=$hook_out)"
fi

# ...and stays silent at session-end, which is the moment the user can least
# act on it — and never spends 30 seconds downloading.
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  XDG_DATA_HOME="$hook_scratch/data" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  bash "$ROOT/hooks/hook.sh" session-end 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ -z "$hook_out" && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hook.sh is silent at session-end with no binary"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh was not silent at session-end (exit=$hook_exit, out=$hook_out)"
fi
rm -rf "$hook_scratch"

# A standalone install at a different version gets named once, with the exact
# command that converges it — the full path, because that standalone binary's
# own `doctor` may predate the fixing step.
#
# Deviation from the brief: its fixture put the resolvable binary at
# "$hook_scratch/bin/stub", a path nothing in hook.sh's or the launcher's
# resolution ever reads. With CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 and no binary
# at the real launcher-resolved path ($XDG_DATA_HOME/cc-loadout/bin/<pin>/),
# resolution failed, hook.sh took its early "could not provision" exit, and
# the standalone-install check below was never reached — confirmed by hand
# (see task-5-report.md) that placing the fixture at the actual resolved path
# makes the hint print correctly. Fixed here by using the real pin so
# --print-path succeeds without a download.
hook_scratch="$(mktemp -d)"
pin="$(tr -d '[:space:]' < "$ROOT/.claude-plugin/cli-version")"
mkdir -p "$hook_scratch/link" "$hook_scratch/data/cc-loadout/bin/$pin"
printf '#!/bin/sh\necho "cc-loadout 0.0.1"\n' > "$hook_scratch/link/cc-loadout"
chmod +x "$hook_scratch/link/cc-loadout"
printf '#!/bin/sh\nexit 0\n' > "$hook_scratch/data/cc-loadout/bin/$pin/cc-loadout"
chmod +x "$hook_scratch/data/cc-loadout/bin/$pin/cc-loadout"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  XDG_DATA_HOME="$hook_scratch/data" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 \
  bash "$ROOT/hooks/hook.sh" session-start 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ "$hook_out" == *"doctor --fix"* && "$hook_out" == *"0.0.1"* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a stale standalone install is named with its converge command"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: no converge hint for a stale standalone install (out=$hook_out)"
fi

# Our own symlink is not a standalone install — no nag. The cheap [ -L ] test
# exists precisely so this case costs nothing. Pointed at the same resolvable
# binary as above (rather than the brief's separate hardcoded "9.9.9" dir) so
# the only variable that changes between this case and the one above is
# symlink-ness, not whether resolution even succeeds.
rm -f "$hook_scratch/link/cc-loadout"
ln -s "$hook_scratch/data/cc-loadout/bin/$pin/cc-loadout" "$hook_scratch/link/cc-loadout"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin \
  XDG_DATA_HOME="$hook_scratch/data" CC_LOADOUT_LINK_DIR="$hook_scratch/link" \
  CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 \
  bash "$ROOT/hooks/hook.sh" session-start 2>/dev/null)" && hook_exit=0 || hook_exit=$?
if [[ "$hook_out" != *"doctor --fix"* ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: our own symlink is not mistaken for a standalone install"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh nagged about its own symlink (out=$hook_out)"
fi
rm -rf "$hook_scratch"

# The launcher resolves this pin to a GitHub Release tarball, so a stale pin
# means a user installs a new plugin and silently runs the old binary.
pin_file="$ROOT/.claude-plugin/cli-version"
pin_ver="$(tr -d '[:space:]' < "$pin_file" 2>/dev/null || true)"
if [[ -n "$cargo_ver" && "$cargo_ver" == "$pin_ver" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: cli-version pin matches Cargo.toml ($cargo_ver)"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: cli-version pin ($pin_ver) != Cargo.toml ($cargo_ver)"
fi

# sync-versions.sh is the only thing that keeps the three files in step; if it
# reports drift here, the tree is already broken.
if bash "$ROOT/scripts/sync-versions.sh" --check >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: sync-versions.sh --check passes"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: sync-versions.sh --check reports drift"
fi
