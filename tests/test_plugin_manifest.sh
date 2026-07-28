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
