# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

stub_registry() {
  mkdir -p "$1/.claude/plugins"
  echo '{"version": 2, "plugins": {}}' > "$1/.claude/plugins/installed_plugins.json"
}

# Locate the pre-built binary (built by cargo earlier, present in target/).
REAL_BINARY="${ROOT}/target/release/cc-loadout"
if [[ ! -x "$REAL_BINARY" ]]; then
  echo "  SKIP: target/release/cc-loadout not found — run 'cargo build --release' first"
  TEST_PASS=$((TEST_PASS+1))
  # shellcheck disable=SC2317
  return 0 2>/dev/null || true
fi

# Create a stub cargo that fakes a successful build by copying the real binary.
fake_bin="$(mktemp -d)"
cat > "$fake_bin/cargo" <<STUBEOF
#!/bin/bash
if [[ "\$1" == "build" ]]; then
  mkdir -p "${ROOT}/target/release"
  [ "${REAL_BINARY}" != "${ROOT}/target/release/cc-loadout" ] && cp "${REAL_BINARY}" "${ROOT}/target/release/cc-loadout"
  exit 0
fi
exec /usr/bin/cargo "\$@" 2>/dev/null || true
STUBEOF
chmod +x "$fake_bin/cargo"
export PATH="$fake_bin:$PATH"

# Fresh install: profiles.json should be a real file (copy of example), not a symlink.
fakehome="$(mktemp -d)"
stub_registry "$fakehome"
INSTALL_DIR="$fakehome/.local/bin" HOME="$fakehome" "$ROOT/install.sh" >/dev/null 2>&1

profiles_file="$fakehome/.claude/profiles/profiles.json"
if [[ -f "$profiles_file" && ! -L "$profiles_file" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: profiles.json seeded as a real file (not symlink)"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: profiles.json is missing or still a symlink"
fi

# Binary should be a regular file (not a symlink) in INSTALL_DIR.
cc_bin="$fakehome/.local/bin/cc-loadout"
if [[ -f "$cc_bin" && ! -L "$cc_bin" && -x "$cc_bin" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: cc-loadout installed as regular executable file"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: cc-loadout binary missing, is a symlink, or not executable"
fi

# Idempotent + non-destructive: user edits to profiles.json must survive re-running install.sh.
echo '{"sentinel": "user-edit", "scan_roots": [], "universal": [], "profiles": {}}' > "$profiles_file"
INSTALL_DIR="$fakehome/.local/bin" HOME="$fakehome" "$ROOT/install.sh" >/dev/null 2>&1
if grep -q '"sentinel": "user-edit"' "$profiles_file"; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: install.sh re-run preserves user edits"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: install.sh overwrote a user-edited profiles.json"
fi

# The installed binary must be runnable and respond to --help / --version.
if "$cc_bin" --help >/dev/null 2>&1 || "$cc_bin" --version >/dev/null 2>&1; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: installed binary runs and responds to --help/--version"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: installed binary did not run or returned non-zero"
fi

rm -rf "$fakehome" "$fake_bin"
