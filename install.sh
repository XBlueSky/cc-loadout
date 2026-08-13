#!/bin/bash
# Installer for cc-loadout (Rust). Two modes:
#   1. In-repo (a clone with .git): builds with cargo, falling back to the
#      published binary if no toolchain is present.
#   2. Otherwise: downloads a pre-built binary from GitHub Releases.
# Either way it then runs `cc-loadout doctor --fix`, which seeds
# ~/.claude/profiles/profiles.json, promotes managed plugins to scope: user, and
# clears any hook entries older versions wrote into ~/.claude/settings.json.
# The SessionStart/SessionEnd hooks themselves now ship with the plugin.
set -e

GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"
REPO="${REPO:-xbluesky/cc-loadout}"
# Overridable the same way scripts/launcher.sh's CC_LOADOUT_RELEASE_BASE is,
# so the test suite can redirect this at a local file:// fixture instead of
# hitting the real network — see install_from_release's proto_args below,
# which relaxes the HTTPS-only guard the same way and for the same reason.
RELEASES_BASE="${CC_LOADOUT_RELEASE_BASE:-https://github.com/${REPO}/releases}"
BINARY_NAME="cc-loadout"
# Must match scripts/launcher.sh exactly, or the plugin and the installer will
# each maintain their own copy and the hook will nag about the other one.
DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}/cc-loadout"
LINK_DIR="${CC_LOADOUT_LINK_DIR:-${INSTALL_DIR}}"
BOOTSTRAP_BIN=""

step() { echo -e "${GREEN}[+]${NC} $1"; }
info() { echo -e "${BLUE}[i]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
err()  { echo -e "${RED}[x]${NC} $1" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

# Source mode means "a real developer clone". Cargo.toml alone is not enough:
# marketplace.json declares `"source": "./"`, so the ENTIRE repo — Cargo.toml,
# src/, install.sh — is copied into the plugin cache, which has no .git. Testing
# for Cargo.toml alone made `bash ${CLAUDE_PLUGIN_ROOT}/install.sh` demand a Rust
# toolchain on the path that is now the front door.
detect_mode() {
  # `-e` not `-d`: in a git worktree `.git` is a regular file holding a
  # `gitdir:` pointer, and a worktree is still a developer clone.
  if [ -f "${REPO_ROOT}/Cargo.toml" ] \
     && [ -e "${REPO_ROOT}/.git" ] \
     && grep -q 'name = "cc-loadout"' "${REPO_ROOT}/Cargo.toml" 2>/dev/null; then
    echo source
  else
    echo binary
  fi
}

# Returns non-zero when it could not produce a binary, so main() can fall back to
# the published release rather than dead-ending someone without a toolchain.
build_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    warn "cargo (Rust toolchain) not found — falling back to the published binary"
    return 1
  fi
  step "Building cc-loadout from source..."
  if ! ( cd "$REPO_ROOT" && cargo build --release ); then
    warn "cargo build failed — falling back to the published binary"
    return 1
  fi
  local bin="${REPO_ROOT}/target/release/${BINARY_NAME}"
  if [ ! -f "$bin" ]; then
    warn "build reported success but produced no binary — falling back to the published binary"
    return 1
  fi
  mkdir -p "$INSTALL_DIR"
  # NOT `cp "$bin" "${INSTALL_DIR}/${BINARY_NAME}"`: cp onto an existing
  # symlink follows it and overwrites the TARGET's contents rather than
  # replacing the link entry. After this branch, INSTALL_DIR defaults to the
  # same ~/.local/bin the plugin's launcher symlinks into
  # $DATA_DIR/bin/<version>/cc-loadout — so a developer with the plugin
  # active who runs this from a clone would have `cp` silently overwrite the
  # checksum-verified pinned release binary with their uncommitted build,
  # with nothing anywhere left to notice. Stage in a fresh temp file (mktemp
  # guarantees it is a real file, never a symlink) and `mv -f` it into place
  # instead — the same idiom install_from_release uses below, because `mv`
  # replaces whatever directory entry is at the destination rather than
  # dereferencing it.
  local tmp_bin; tmp_bin="$(mktemp "${INSTALL_DIR}/.${BINARY_NAME}.XXXXXX")"
  cp "$bin" "$tmp_bin"
  chmod +x "$tmp_bin"
  mv -f "$tmp_bin" "${INSTALL_DIR}/${BINARY_NAME}"
  BOOTSTRAP_BIN="${INSTALL_DIR}/${BINARY_NAME}"
  info "Installed ${INSTALL_DIR}/${BINARY_NAME}"
  # Deliberately a real file, NOT $DATA_DIR/bin/<version>/: a dev build parked
  # there is indistinguishable from the released build of that version, and
  # every session would silently run uncommitted code. Developers who want the
  # plugin to use this build should say so explicitly.
  info "To make the plugin use this build: export CC_LOADOUT_BIN=${INSTALL_DIR}/${BINARY_NAME}"
}

# Map this machine to one of the release asset targets. Anything else has to
# build from source — there is no published binary to fall back to. The mapping
# matches cc-uplink's plugin launcher so both projects resolve the same way.
detect_target() {
  local os arch
  os="$(uname -s)"; arch="$(uname -m)"
  case "${os}-${arch}" in
    Linux-x86_64)   echo "x86_64-unknown-linux-musl" ;;
    Linux-aarch64)  echo "aarch64-unknown-linux-musl" ;;
    Darwin-arm64)   echo "aarch64-apple-darwin" ;;
    Darwin-x86_64)  echo "x86_64-apple-darwin" ;;
    *) err "no published binary for ${os}/${arch}; build from source instead: git clone https://github.com/${REPO} && cd cc-loadout && ./install.sh" ;;
  esac
}

# Resolve the newest tag by following the /releases/latest redirect, so this
# needs neither jq nor an API token.
latest_version() {
  curl -sSL -o /dev/null -w '%{url_effective}' "${RELEASES_BASE}/latest" \
    | sed -n 's|.*/tag/v\(.*\)$|\1|p'
}

# Where this run's backup will go. Mirrors src/doctor.rs's
# standalone_backup_path exactly, including the naming scheme, so the two
# tools can never fight over a name: never clobbers a previous convergence's
# backup. If cc-loadout.standalone.bak is already occupied — a second regular
# file landed at $link after an earlier `doctor --fix` or `install.sh` run
# already converged one — a bare `mv -f` onto that fixed name would silently
# destroy the earlier backup with no way back. The next free
# cc-loadout.standalone.bak.<n> is used instead; the numeral lives in the
# `.standalone.bak` stem, not after a `.bak.` of its own, so every candidate
# still fails doctor's `cc-loadout.bak.` prune-backups prefix test and none of
# them is ever swept by `--prune-backups`.
standalone_backup_path() {
  local link="$1" dir base candidate n
  dir="$(dirname "$link")"
  base="${dir}/${BINARY_NAME}.standalone.bak"
  # `[ -e ] || [ -L ]`: -e alone follows a symlink and reads false for a
  # dangling one, which would make an occupied-but-dangling name look free and
  # get silently overwritten below. Same reasoning as doctor.rs's
  # symlink_metadata over exists().
  if [ ! -e "$base" ] && [ ! -L "$base" ]; then
    echo "$base"
    return
  fi
  n=1
  while :; do
    candidate="${dir}/${BINARY_NAME}.standalone.bak.${n}"
    if [ ! -e "$candidate" ] && [ ! -L "$candidate" ]; then
      echo "$candidate"
      return
    fi
    n=$((n + 1))
  done
}

install_from_release() {
  step "Downloading cc-loadout from GitHub Releases..."
  command -v curl >/dev/null 2>&1 || err "curl not found"
  command -v tar  >/dev/null 2>&1 || err "tar not found"
  local target; target="$(detect_target)"
  local version="${VERSION:-$(latest_version)}"
  [ -n "$version" ] || err "could not determine the latest version (set VERSION=...)"
  info "Version: ${version} (${target})"

  local asset="${BINARY_NAME}-${target}.tar.gz"
  # The checksum file is named without .tar.gz but its body references the
  # archive, so `-c` works from the download dir.
  local sum="${BINARY_NAME}-${target}.sha256"
  local base="${RELEASES_BASE}/download/v${version}"

  local tmp; tmp="$(mktemp -d)"
  # shellcheck disable=SC2064  # expand $tmp now, not at trap time
  trap "rm -rf '$tmp'" EXIT

  # HTTPS is enforced for the real release host and relaxed only when
  # CC_LOADOUT_RELEASE_BASE is explicitly overridden, which the test suite
  # does with file:// fixtures (curl's --proto =https rejects file:// outright).
  # Keying on the override — not on the URL scheme — keeps the default path
  # incapable of being downgraded by anything a redirect could say. Same
  # reasoning as scripts/launcher.sh's identical guard.
  local proto_args=(--proto '=https' --proto-redir '=https')
  [ -z "${CC_LOADOUT_RELEASE_BASE:-}" ] || proto_args=()

  # -f so an HTTP error page is a failure instead of a "binary" written to disk.
  curl -fsSL "${proto_args[@]}" -o "${tmp}/${asset}" "${base}/${asset}" \
    || err "download failed for ${asset} at v${version}"
  curl -fsSL "${proto_args[@]}" -o "${tmp}/${sum}" "${base}/${sum}" \
    || err "download failed for ${sum} at v${version}"

  # Verify BEFORE extracting anything from the archive. The checksum ships
  # beside the tarball, so it guards download integrity — not authenticity;
  # the trust root is the pinned version plus HTTPS.
  step "Verifying checksum..."
  if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$tmp" && sha256sum -c "$sum" ) >/dev/null || err "checksum mismatch for ${asset}"
  elif command -v shasum >/dev/null 2>&1; then
    ( cd "$tmp" && shasum -a 256 -c "$sum" ) >/dev/null || err "checksum mismatch for ${asset}"
  else
    err "neither sha256sum nor shasum found — cannot verify the download"
  fi

  tar -xzf "${tmp}/${asset}" -C "$tmp" "${BINARY_NAME}" || err "could not extract ${BINARY_NAME} from ${asset}"
  local vdir="${DATA_DIR}/bin/${version}"
  mkdir -p "$vdir" "$LINK_DIR"
  chmod +x "${tmp}/${BINARY_NAME}"
  mv -f "${tmp}/${BINARY_NAME}" "${vdir}/${BINARY_NAME}"
  rm -rf "$tmp"; trap - EXIT

  # Same layout the plugin launcher maintains, so the two front doors converge
  # on one binary instead of leaving two at different versions. -n so an
  # existing symlink-to-a-directory is replaced rather than followed into.
  local link="${LINK_DIR}/${BINARY_NAME}"
  if [ -e "$link" ] && [ ! -L "$link" ]; then
    local backup; backup="$(standalone_backup_path "$link")"
    mv -f "$link" "$backup"
    warn "moved the previous ${link} to $(basename "$backup")"
  fi
  ln -sfn "${vdir}/${BINARY_NAME}" "$link"
  info "Installed ${vdir}/${BINARY_NAME}"
  info "Linked ${link}"
  BOOTSTRAP_BIN="${vdir}/${BINARY_NAME}"
}

# Bootstrap now runs in BOTH modes, because the binary owns it: seeding
# profiles.json, promoting plugin scope, and clearing the retired settings.json
# hooks are all `cc-loadout doctor --fix`. Hooks themselves are no longer
# installed here at all — the bundled plugin ships them.
bootstrap() {
  step "Running cc-loadout bootstrap..."
  # The just-installed binary by absolute path, not by name: $INSTALL_DIR may
  # not be on PATH yet (see main()'s warning), and in binary mode the real
  # binary now lives under the data dir.
  "${BOOTSTRAP_BIN}" doctor --fix || warn "doctor reported an issue"
}

main() {
  # An unknown option must be a hard error, not a fall-through to a real
  # install: that fall-through is exactly what happened once during this
  # project's own development, running a full install against a developer's
  # real environment (see tests/test_install.sh's isolation comment above
  # print_mode()). An explicit case makes "only $1 is ever consulted" a
  # stated contract too, not an accident of a single `if`.
  case "${1:-}" in
    --print-mode) detect_mode; exit 0 ;;
    "") ;;
    *) err "unknown option: $1 (supported: --print-mode)" ;;
  esac
  echo -e "${BLUE}cc-loadout installer${NC}"
  local mode; mode="$(detect_mode)"
  info "Mode: $mode"
  if [ "$mode" = source ]; then
    build_from_source || install_from_release
  else
    install_from_release
  fi
  bootstrap
  echo ""
  step "Done. Try: ${BINARY_NAME} --help"
  echo "$PATH" | grep -q "$LINK_DIR" || warn "add $LINK_DIR to PATH: export PATH=\"${LINK_DIR}:\${PATH}\""
}

main "$@"
