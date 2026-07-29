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

# Regression guard: this file also checks hook.sh *behaviour*, not just
# manifest shape, because the binary-resolution lookup it validates has no
# other test coverage. A non-executable cc-loadout on PATH must be treated
# the same as no cc-loadout at all: `command -v` alone reports a match
# without checking the executable bit, so a partial/broken install must not
# silently swallow the one message that explains it.
hook_scratch="$(mktemp -d)"
mkdir -p "$hook_scratch/bin"
printf '#!/bin/bash\necho ran\n' > "$hook_scratch/bin/cc-loadout"
chmod 644 "$hook_scratch/bin/cc-loadout"
hook_out="$(echo '{}' | env -i HOME=/nonexistent PATH="$hook_scratch/bin:/usr/bin:/bin" bash "$ROOT/hooks/hook.sh" session-start 2>&1)"
hook_exit=$?
if [[ "$hook_out" == *"cc-loadout: CLI not installed"* && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hook.sh treats a non-executable cc-loadout on PATH as missing"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh did not print the install hint for a non-executable cc-loadout (exit=$hook_exit, out=$hook_out)"
fi
rm -rf "$hook_scratch"

# Regression guard: `-x` alone returns true for a directory (the traversal
# bit is set by default), so the ~/.local/bin fallback must also check `-f`
# or a stray directory named cc-loadout is treated as usable.
hook_scratch="$(mktemp -d)"
mkdir -p "$hook_scratch/.local/bin/cc-loadout"
hook_out="$(echo '{}' | env -i HOME="$hook_scratch" PATH=/usr/bin:/bin bash "$ROOT/hooks/hook.sh" session-start 2>&1)"
hook_exit=$?
if [[ "$hook_out" == *"cc-loadout: CLI not installed"* && $hook_exit -eq 0 ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: hook.sh treats a directory at the fallback path as missing"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: hook.sh did not print the install hint for a directory at \$HOME/.local/bin/cc-loadout (exit=$hook_exit, out=$hook_out)"
fi
rm -rf "$hook_scratch"
