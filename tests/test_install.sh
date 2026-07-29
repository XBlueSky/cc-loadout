# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

stub_registry() {
  mkdir -p "$1/.claude/plugins"
  echo '{"version": 2, "plugins": {}}' > "$1/.claude/plugins/installed_plugins.json"
}

REAL_BINARY="${ROOT}/target/release/cc-loadout"
if [[ ! -x "$REAL_BINARY" ]]; then
  echo "  SKIP: target/release/cc-loadout not found — run 'cargo build --release' first"
  TEST_PASS=$((TEST_PASS+1))
  # shellcheck disable=SC2317
  return 0 2>/dev/null || true
fi

# --- mode detection -------------------------------------------------------
# --print-mode is defined to exit before touching HOME at all, but these two
# invocations still isolate HOME/XDG_DATA_HOME defensively: an agent running
# this suite against an install.sh that does NOT yet understand --print-mode
# (i.e. mid-refactor, before Step 3 lands) will fall through to the real
# main() and install into whatever HOME is set — which, unisolated, is the
# developer's real ~/.claude. That happened once during this task's own
# development. See task-6-report.md for the incident.
modehome="$(mktemp -d)"

# A real clone (Cargo.toml + .git) is source mode.
assert_eq "$(HOME="$modehome" XDG_DATA_HOME="$modehome/.local/share" bash "$ROOT/install.sh" --print-mode)" "source" \
  "a clone with .git is detected as source mode"

# A plugin-cache-shaped copy (Cargo.toml, no .git) must NOT demand a toolchain.
cachedir="$(mktemp -d)"
cp "$ROOT/install.sh" "$cachedir/"
cp "$ROOT/Cargo.toml" "$cachedir/"
assert_eq "$(HOME="$modehome" XDG_DATA_HOME="$modehome/.local/share" bash "$cachedir/install.sh" --print-mode)" "binary" \
  "a plugin-cache copy without .git is detected as binary mode"
rm -rf "$cachedir" "$modehome"

# --- bootstrap ------------------------------------------------------------
fake_bin="$(mktemp -d)"
cat > "$fake_bin/cargo" <<STUBEOF
#!/bin/bash
if [[ "\$1" == "build" ]]; then
  mkdir -p "${ROOT}/target/release"
  exit 0
fi
exec /usr/bin/cargo "\$@" 2>/dev/null || true
STUBEOF
chmod +x "$fake_bin/cargo"
export PATH="$fake_bin:$PATH"

fakehome="$(mktemp -d)"
stub_registry "$fakehome"
# CC_LOADOUT_PROFILES is cleared (not just left unset): helpers.sh, sourced
# above, exports it for the fixture-based tests elsewhere in this suite, and
# it would otherwise leak into this child process and make `doctor --fix`
# seed the fixture path instead of $fakehome — profiles_path() prefers a
# non-empty override over $HOME. Setting it to "" makes profiles_path() treat
# it as absent (see profile/config.rs's `.filter(|s| !s.is_empty())`).
INSTALL_DIR="$fakehome/.local/bin" HOME="$fakehome" XDG_DATA_HOME="$fakehome/.local/share" CC_LOADOUT_PROFILES="" "$ROOT/install.sh" >/dev/null 2>&1

profiles_file="$fakehome/.claude/profiles/profiles.json"
if [[ -f "$profiles_file" && ! -L "$profiles_file" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: profiles.json seeded as a real file (not symlink)"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: profiles.json is missing or still a symlink"
fi

cc_bin="$fakehome/.local/bin/cc-loadout"
if [[ -f "$cc_bin" && ! -L "$cc_bin" && -x "$cc_bin" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: cc-loadout installed as regular executable file"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: cc-loadout binary missing, is a symlink, or not executable"
fi

# The installer must no longer touch settings.json — the plugin owns the hooks.
settings="$fakehome/.claude/settings.json"
if [[ ! -f "$settings" ]] || ! grep -q "session-start-hook.sh" "$settings" 2>/dev/null; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh wrote no hook into settings.json"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh still installs a settings.json hook"
fi

# Idempotent + non-destructive.
echo '{"sentinel": "user-edit", "scan_roots": [], "universal": [], "profiles": {}}' > "$profiles_file"
# CC_LOADOUT_PROFILES cleared again — see the comment on the first bootstrap run above.
INSTALL_DIR="$fakehome/.local/bin" HOME="$fakehome" XDG_DATA_HOME="$fakehome/.local/share" CC_LOADOUT_PROFILES="" "$ROOT/install.sh" >/dev/null 2>&1
if grep -q '"sentinel": "user-edit"' "$profiles_file"; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh re-run preserves user edits"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh overwrote a user-edited profiles.json"
fi

if "$cc_bin" --help >/dev/null 2>&1 || "$cc_bin" --version >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: installed binary runs and responds to --help/--version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: installed binary did not run or returned non-zero"
fi

rm -rf "$fakehome" "$fake_bin"
