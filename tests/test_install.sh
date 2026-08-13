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
# Isolation here must NOT be conditional on install.sh already having the
# --print-mode fix: an assertion whose safety depends on the code under test
# already being correct is not a safe assertion. --print-mode is defined to
# exit before touching HOME at all, but only once install.sh actually
# understands the flag — against the old install.sh (mid-refactor, before
# Step 3 lands) an unknown flag falls through to the real main(), which
# installs into whatever HOME is set. Unisolated, that is the developer's
# real ~/.claude, and it happened once during this task's own development
# (see task-6-report.md). So print_mode() isolates HOME/XDG_DATA_HOME/
# INSTALL_DIR to a scratch dir unconditionally, on every call, regardless of
# whether the fix is in place yet.
print_mode() {
  local install_sh="$1"
  local s; s="$(mktemp -d)"
  local out
  out="$(INSTALL_DIR="$s/.local/bin" HOME="$s" XDG_DATA_HOME="$s/data" bash "$install_sh" --print-mode)"
  rm -rf "$s"
  echo "$out"
}

# A real clone (Cargo.toml + .git) is source mode.
assert_eq "$(print_mode "$ROOT/install.sh")" "source" \
  "a clone with .git is detected as source mode"

# A plugin-cache-shaped copy (Cargo.toml, no .git) must NOT demand a toolchain.
cachedir="$(mktemp -d)"
cp "$ROOT/install.sh" "$cachedir/"
cp "$ROOT/Cargo.toml" "$cachedir/"
assert_eq "$(print_mode "$cachedir/install.sh")" "binary" \
  "a plugin-cache copy without .git is detected as binary mode"
rm -rf "$cachedir"

# --- unknown flag -----------------------------------------------------------
# An unknown option must fail loudly rather than falling through to a real
# install — that fall-through is breach #2 of this branch's own development
# (an installer that ignored an unknown flag ran a full install against a
# developer's real environment). Isolated unconditionally, same as
# print_mode() above, so this assertion never depends on the fix already
# being correct.
s="$(mktemp -d)"
if INSTALL_DIR="$s/.local/bin" HOME="$s" XDG_DATA_HOME="$s/data" \
  bash "$ROOT/install.sh" --bogus-flag >/dev/null 2>&1; then
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh accepted an unknown flag instead of erroring"
else
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh errors on an unknown flag"
fi
if [[ -e "$s/.claude" ]]; then
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh performed a real install despite the unknown flag"
else
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh did not touch HOME when handed an unknown flag"
fi
rm -rf "$s"

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

# HOME/XDG_DATA_HOME isolation here is provably inert today — Cli::parse()
# handles --help/--version and exits before run() ever reaches resolve_env(),
# the only place $HOME is read — but that safety comes from an implementation
# detail one layer down, not from this test's own isolation. A test must not
# depend on the correctness of anything it is testing, nor on a contract in a
# library it does not control (the same class of gap as the --print-mode
# incident above). Reuses $fakehome, since $cc_bin already lives inside it.
if HOME="$fakehome" XDG_DATA_HOME="$fakehome/.local/share" "$cc_bin" --help >/dev/null 2>&1 \
  || HOME="$fakehome" XDG_DATA_HOME="$fakehome/.local/share" "$cc_bin" --version >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: installed binary runs and responds to --help/--version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: installed binary did not run or returned non-zero"
fi

rm -rf "$fakehome" "$fake_bin"

# --- shared layout with scripts/launcher.sh --------------------------------

# Binary mode must produce the same layout as scripts/launcher.sh: the binary
# under the data dir at its own version, plus a symlink on PATH. Otherwise
# every install.sh user with the plugin installed lands in hook.sh's
# converge-hint path, and the migration nag becomes the normal experience.
if grep -q 'XDG_DATA_HOME' "$ROOT/install.sh" && grep -q 'ln -sfn' "$ROOT/install.sh"; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh knows the data-dir + symlink layout"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh still installs a bare file on PATH"
fi

# Source mode must NOT write into the data dir: a dev build sitting at
# $DATA/bin/<version>/ is indistinguishable from the released build of that
# version, and every session would then silently run uncommitted code.
if grep -q 'CC_LOADOUT_BIN' "$ROOT/install.sh"; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh points developers at CC_LOADOUT_BIN"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh does not mention the dev override"
fi
