#!/bin/bash
# Installer for cc-loadout (Rust). Two modes:
#   1. In-repo: builds with cargo and installs the binary.
#   2. curl | bash: downloads a pre-built binary from GitHub Releases.
# After installing the binary it runs the cc-loadout bootstrap:
#   - seed ~/.claude/profiles/profiles.json from profiles.example.json (if absent)
#   - promote universal / on-demand / profile plugins to scope: user
#   - install the SessionStart hook (re-enforces on every session)
set -e

GREEN='\033[0;32m'; BLUE='\033[0;34m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
INSTALL_DIR="${INSTALL_DIR:-${HOME}/.local/bin}"
REPO="${REPO:-xbluesky/cc-loadout}"
RELEASES_BASE="https://github.com/${REPO}/releases"
BINARY_NAME="cc-loadout"

step() { echo -e "${GREEN}[+]${NC} $1"; }
info() { echo -e "${BLUE}[i]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
err()  { echo -e "${RED}[x]${NC} $1" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

detect_mode() {
  if [ -f "${REPO_ROOT}/Cargo.toml" ] && grep -q 'name = "cc-loadout"' "${REPO_ROOT}/Cargo.toml" 2>/dev/null; then
    echo source
  else
    echo binary
  fi
}

build_from_source() {
  step "Building cc-loadout from source..."
  command -v cargo >/dev/null 2>&1 || err "cargo (Rust toolchain) not found"
  ( cd "$REPO_ROOT" && cargo build --release ) || err "cargo build failed"
  local bin="${REPO_ROOT}/target/release/${BINARY_NAME}"
  [ -f "$bin" ] || err "build succeeded but binary missing: $bin"
  mkdir -p "$INSTALL_DIR"
  cp "$bin" "${INSTALL_DIR}/${BINARY_NAME}"
  chmod +x "${INSTALL_DIR}/${BINARY_NAME}"
  info "Installed ${INSTALL_DIR}/${BINARY_NAME}"
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

  # -f so an HTTP error page is a failure instead of a "binary" written to disk.
  curl -fsSL --proto '=https' --proto-redir '=https' -o "${tmp}/${asset}" "${base}/${asset}" \
    || err "download failed for ${asset} at v${version}"
  curl -fsSL --proto '=https' --proto-redir '=https' -o "${tmp}/${sum}" "${base}/${sum}" \
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
  mkdir -p "$INSTALL_DIR"
  chmod +x "${tmp}/${BINARY_NAME}"
  mv -f "${tmp}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
  rm -rf "$tmp"; trap - EXIT
  info "Installed ${INSTALL_DIR}/${BINARY_NAME}"
}

bootstrap() {
  # Only runs in-repo (needs profiles.example.json + lib/registry.sh).
  local mode="$1"
  [ "$mode" = "source" ] || { info "binary mode: skipping repo bootstrap"; return 0; }

  step "Running cc-loadout bootstrap..."
  local profiles="${HOME}/.claude/profiles/profiles.json"
  if [ ! -f "$profiles" ]; then
    mkdir -p "$(dirname "$profiles")"
    cp "${REPO_ROOT}/profiles.example.json" "$profiles"
    info "seeded $profiles from template"
  else
    info "profiles.json exists — left untouched"
  fi

  # shellcheck source=lib/registry.sh
  source "${REPO_ROOT}/lib/registry.sh"
  promote_universal_to_user || warn "promote_universal_to_user reported an issue"
  promote_on_demand_to_user || warn "promote_on_demand_to_user reported an issue"
  promote_profiles_to_user || warn "promote_profiles_to_user reported an issue"
  install_session_hook || warn "install_session_hook reported an issue"
  install_session_end_hook || warn "install_session_end_hook reported an issue"
}

main() {
  echo -e "${BLUE}cc-loadout installer${NC}"
  local mode; mode="$(detect_mode)"
  info "Mode: $mode"
  if [ "$mode" = source ]; then build_from_source; else install_from_release; fi
  bootstrap "$mode"
  echo ""
  step "Done. Try: ${BINARY_NAME} --help"
  echo "$PATH" | grep -q "$INSTALL_DIR" || warn "add $INSTALL_DIR to PATH: export PATH=\"${INSTALL_DIR}:\${PATH}\""
}

main "$@"
