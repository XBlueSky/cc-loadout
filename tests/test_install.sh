# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

stub_registry() {
  mkdir -p "$1/.claude/plugins"
  echo '{"version": 2, "plugins": {}}' > "$1/.claude/plugins/installed_plugins.json"
}

REAL_BINARY="${ROOT}/target/release/cc-loadout"
# CI's shell-tests job (.github/workflows/ci.yml) runs this suite with no
# native release build at all — the only `cargo build --release` in CI
# targets a musl/aarch64 triple in a separate job, writing
# target/<triple>/release/, never target/release/. This used to bump
# TEST_PASS and `return` before a single assertion in this file ran: a skip
# is not a pass, and doing it that way silently skipped every assertion below
# (not just the ones that actually need a real binary) while still reporting
# green. HAVE_REAL_BINARY instead gates only the two blocks further down that
# truly cannot run without one — both stub `cargo build` to produce no
# binary, so without a real one already at $REAL_BINARY they fall through to
# install_from_release() and hit the real network. Everything else in this
# file (mode detection, the unknown-flag guard, the layout greps, and the
# file://-fixture-driven install_from_release coverage) needs only bash,
# curl, tar and a shasum tool, and now runs unconditionally, in CI included.
HAVE_REAL_BINARY=0
if [[ -x "$REAL_BINARY" ]]; then
  HAVE_REAL_BINARY=1
else
  echo "  SKIP: target/release/cc-loadout not found (run 'cargo build --release' first) — the bootstrap and build_from_source-symlink blocks below need it to stay off the network, and are skipped, not passed"
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
# Needs $REAL_BINARY: the stubbed cargo below produces the target/release/
# directory but no binary in it, so build_from_source()'s own `[ -f "$bin" ]`
# check only passes if a real build already sits there from before this test
# ran. Without one, it falls through to install_from_release() and reaches
# the actual GitHub network — exactly the "ran a full install against a
# developer's real environment" failure mode this suite's other isolation
# comments describe, just via the network path instead of $HOME.
if [[ $HAVE_REAL_BINARY -eq 1 ]]; then
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
else
  echo "  SKIP: bootstrap block needs \$REAL_BINARY (see HAVE_REAL_BINARY above)"
fi

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

# --- build_from_source must not follow a symlink at INSTALL_DIR ------------
# Task-7 review Critical: `cp SRC DST` where DST is an existing symlink
# follows the link and overwrites the TARGET's contents rather than replacing
# the link entry. After this branch, INSTALL_DIR defaults to the same
# ~/.local/bin the plugin's launcher symlinks into
# $DATA_DIR/bin/<version>/cc-loadout — so a developer with the plugin active
# who ran `./install.sh` from a clone had `cp` silently overwrite the
# checksum-verified pinned release binary with their uncommitted build, with
# nothing anywhere left to notice (hooks/hook.sh's standalone-install hint
# never fires, because the link path is still a symlink, not a real file).
#
# Exercised with the same stubbed-cargo idiom as the bootstrap block above —
# its own $fake_bin/$fakehome were already torn down, so this uses a fresh
# pair and restores $PATH afterward rather than depending on whatever cargo
# this machine happens to have.
#
# Needs $REAL_BINARY for the same reason the bootstrap block does: the
# stubbed cargo makes build_from_source() believe the build succeeded, but
# only a real binary already at $REAL_BINARY makes its `[ -f "$bin" ]` check
# actually pass. Without one this falls through to install_from_release()
# and the real network, same as the bootstrap block above.
if [[ $HAVE_REAL_BINARY -eq 1 ]]; then
fake_bin2="$(mktemp -d)"
cat > "$fake_bin2/cargo" <<STUBEOF
#!/bin/bash
if [[ "\$1" == "build" ]]; then
  mkdir -p "${ROOT}/target/release"
  exit 0
fi
exec /usr/bin/cargo "\$@" 2>/dev/null || true
STUBEOF
chmod +x "$fake_bin2/cargo"
saved_path="$PATH"
export PATH="$fake_bin2:$PATH"

fakehome2="$(mktemp -d)"
stub_registry "$fakehome2"
pinned_dir="$(mktemp -d)"
printf 'PINNED-RELEASE-CONTENT\n' > "$pinned_dir/cc-loadout"
mkdir -p "$fakehome2/.local/bin"
ln -s "$pinned_dir/cc-loadout" "$fakehome2/.local/bin/cc-loadout"

# A bare top-level command whose failure would abort the whole suite under
# `set -euo pipefail` (see tests/run.sh) is guarded the same way every command
# substitution in this file is: `&& rc=0 || rc=$?` keeps a failure local to
# this assertion instead of truncating every test after it.
INSTALL_DIR="$fakehome2/.local/bin" HOME="$fakehome2" XDG_DATA_HOME="$fakehome2/.local/share" \
  CC_LOADOUT_PROFILES="" "$ROOT/install.sh" >/dev/null 2>&1 && rc=0 || rc=$?

if [[ "$(cat "$pinned_dir/cc-loadout")" == "PINNED-RELEASE-CONTENT" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: build_from_source does not overwrite a symlink's target"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: build_from_source followed the symlink and clobbered the pinned binary"
fi
if [[ -f "$fakehome2/.local/bin/cc-loadout" && ! -L "$fakehome2/.local/bin/cc-loadout" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: build_from_source replaces the symlink itself with a real file"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: INSTALL_DIR/cc-loadout is not a real file after install (rc=$rc)"
fi

export PATH="$saved_path"
rm -rf "$fakehome2" "$fake_bin2" "$pinned_dir"
else
  echo "  SKIP: build_from_source-symlink block needs \$REAL_BINARY (see HAVE_REAL_BINARY above)"
fi

# --- install_from_release fixture coverage (file:// override, no network) --
# scripts/launcher.sh's tests redirect downloads at a local file:// fixture
# via CC_LOADOUT_RELEASE_BASE (see tests/test_launcher.sh's make_release_fixture
# and the comment on install.sh's own RELEASES_BASE). install.sh now honours
# the same override, so the real download -> checksum -> placement -> symlink
# path can be exercised here too, with no network and no manual sandbox run.
#
# These helpers mirror tests/test_launcher.sh's identically-named ones rather
# than sharing them: tests/run.sh sources test_install.sh BEFORE
# test_launcher.sh, so the launcher's helpers do not exist yet when this file
# runs, and install.sh's asset URL is one path segment deeper than the
# launcher's fixture layout (RELEASES_BASE already ends in `/releases`, and
# install_from_release appends `/download/v<version>` on top of it).
install_target() {
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   echo x86_64-unknown-linux-musl ;;
    Linux-aarch64)  echo aarch64-unknown-linux-musl ;;
    Darwin-arm64)   echo aarch64-apple-darwin ;;
    Darwin-x86_64)  echo x86_64-apple-darwin ;;
    *) echo unsupported ;;
  esac
}

install_sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

make_install_release_fixture() {
  local dir="$1" ver="$2" target stage
  target="$(install_target)"
  mkdir -p "$dir/download/v$ver"
  stage="$(mktemp -d)"
  printf '#!/bin/sh\necho "cc-loadout %s"\necho "args:$*"\n' "$ver" > "$stage/cc-loadout"
  chmod +x "$stage/cc-loadout"
  ( cd "$stage" && tar -czf "$dir/download/v$ver/cc-loadout-$target.tar.gz" cc-loadout )
  ( cd "$dir/download/v$ver" && install_sha256_of "cc-loadout-$target.tar.gz" > "cc-loadout-$target.sha256" )
  rm -rf "$stage"
}

if [[ "$(install_target)" == unsupported ]]; then
  echo "  skip: install.sh binary-mode fixture tests need a mapped target (this is $(uname -s)/$(uname -m))"
else

# Binary mode places the binary under the data dir and links it on PATH — the
# actual layout the two grep assertions above only check for textually.
scratch="$(mktemp -d)"
cp "$ROOT/install.sh" "$scratch/"
make_install_release_fixture "$scratch/rel" 9.9.9
out="$(CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" VERSION=9.9.9 \
  INSTALL_DIR="$scratch/bin" HOME="$scratch" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_PROFILES="" bash "$scratch/install.sh" 2>&1)" && rc=0 || rc=$?
if [[ $rc -eq 0 && -x "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: binary mode installs the binary under the data dir"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: binary mode did not install under the data dir (rc=$rc)"
  echo "$out"
fi
link_target="$(readlink "$scratch/bin/cc-loadout" 2>/dev/null || true)" && rc=0 || rc=$?
assert_eq "$link_target" "$scratch/data/cc-loadout/bin/9.9.9/cc-loadout" \
  "binary mode links PATH to the versioned binary"
rm -rf "$scratch"

# A pre-existing REAL FILE at the link path is backed up, not destroyed.
scratch="$(mktemp -d)"
cp "$ROOT/install.sh" "$scratch/"
make_install_release_fixture "$scratch/rel" 9.9.9
mkdir -p "$scratch/bin"
printf 'ORIGINAL-STANDALONE-CONTENT\n' > "$scratch/bin/cc-loadout"
chmod +x "$scratch/bin/cc-loadout"
CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" VERSION=9.9.9 \
  INSTALL_DIR="$scratch/bin" HOME="$scratch" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_PROFILES="" bash "$scratch/install.sh" >/dev/null 2>&1 && rc=0 || rc=$?
if [[ -L "$scratch/bin/cc-loadout" \
      && -f "$scratch/bin/cc-loadout.standalone.bak" \
      && "$(cat "$scratch/bin/cc-loadout.standalone.bak")" == "ORIGINAL-STANDALONE-CONTENT" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a pre-existing real file at the link path is backed up, not destroyed"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: pre-existing standalone file was not safely backed up (rc=$rc)"
fi
rm -rf "$scratch"

# A pre-existing .standalone.bak forces the numbered fallback, so a second
# convergence can never destroy the first one's backup.
scratch="$(mktemp -d)"
cp "$ROOT/install.sh" "$scratch/"
make_install_release_fixture "$scratch/rel" 9.9.9
mkdir -p "$scratch/bin"
printf 'FIRST-BACKUP-CONTENT\n' > "$scratch/bin/cc-loadout.standalone.bak"
printf 'SECOND-STANDALONE-CONTENT\n' > "$scratch/bin/cc-loadout"
chmod +x "$scratch/bin/cc-loadout"
CC_LOADOUT_RELEASE_BASE="file://$scratch/rel" VERSION=9.9.9 \
  INSTALL_DIR="$scratch/bin" HOME="$scratch" XDG_DATA_HOME="$scratch/data" \
  CC_LOADOUT_PROFILES="" bash "$scratch/install.sh" >/dev/null 2>&1 && rc=0 || rc=$?
if [[ "$(cat "$scratch/bin/cc-loadout.standalone.bak" 2>/dev/null)" == "FIRST-BACKUP-CONTENT" \
      && -f "$scratch/bin/cc-loadout.standalone.bak.1" \
      && "$(cat "$scratch/bin/cc-loadout.standalone.bak.1")" == "SECOND-STANDALONE-CONTENT" ]]; then
  TEST_PASS=$((TEST_PASS+1)); echo "  ok: a pre-existing .standalone.bak forces the numbered fallback"
else
  TEST_FAIL=$((TEST_FAIL+1)); echo "  FAIL: numbered backup fallback did not trigger correctly (rc=$rc)"
fi
rm -rf "$scratch"

fi  # end mapped-target guard
