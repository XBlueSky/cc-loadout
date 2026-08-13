#!/bin/sh
# cc-loadout plugin launcher — resolves the version-pinned CLI binary.
#
# Runs the release binary pinned in `.claude-plugin/cli-version`, downloading it
# into the plugin data dir on first use so the plugin's skills and the binary
# they drive can never version-skew. Two calling conventions:
#
#   launcher.sh --print-path        resolve, print the path, exit
#   launcher.sh <args...>           resolve, then exec the binary with <args>
#
# Every message goes to stderr. `--print-path` writes ONLY the path to stdout,
# because its caller is a SessionStart hook whose stdout is injected into the
# session's context.
set -eu

# Filled in by later tasks; defined here so the resolution path below is
# complete and testable on its own.
download() {
  echo "cc-loadout launcher: download not implemented" >&2
  exit 1
}
gc_old_versions() { :; }
reconcile_link() { :; }

print_path_only=0
if [ "${1:-}" = "--print-path" ]; then
  print_path_only=1
  shift
fi

# Dev override: run a local build, skip pinning entirely.
if [ -n "${CC_LOADOUT_BIN:-}" ]; then
  if [ "$print_path_only" -eq 1 ]; then
    printf '%s\n' "$CC_LOADOUT_BIN"
    exit 0
  fi
  exec "$CC_LOADOUT_BIN" "$@"
fi

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PIN_FILE="$ROOT/.claude-plugin/cli-version"
VER=$(tr -d '[:space:]' <"$PIN_FILE" 2>/dev/null || true)

# Strict enough to be safe, not a semver parser. $VER is interpolated into a
# filesystem path below, so the second pattern is load-bearing: the first alone
# accepts `1/../../evil.0.0` (glob `*` spans slashes), which would resolve
# outside the data dir. Rejecting every character that is not a digit or a dot
# closes that without needing to enumerate traversal shapes.
pin_ok=0
case "$VER" in
[0-9]*.[0-9]*.[0-9]*)
  case "$VER" in
  *[!0-9.]*) ;;
  *) pin_ok=1 ;;
  esac
  ;;
esac
if [ "$pin_ok" -ne 1 ]; then
  echo "cc-loadout launcher: bad version pin '$VER' in .claude-plugin/cli-version" >&2
  exit 1
fi

# Deliberately NOT $CLAUDE_PLUGIN_DATA, which cc-uplink's launcher prefers.
# Claude Code sets that for MCP servers; whether a hook process receives it is
# not established, and cc-loadout has no MCP server so nothing else will ever
# set it. If it were present in one context and absent in another, the same
# machine would resolve two data dirs, download twice, and flap the PATH
# symlink between them. One deterministic path is strictly better here.
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/cc-loadout"
BIN="$DATA/bin/$VER/cc-loadout"
LINK_DIR="${CC_LOADOUT_LINK_DIR:-$HOME/.local/bin}"
LINK="$LINK_DIR/cc-loadout"

# `-x` and not `-f`: a directory would also pass a bare `-x` (its traversal bit
# is set by default), and a present-but-non-executable file must be treated as
# absent rather than exec'd — an unusable binary is indistinguishable from a
# missing one.
if [ ! -f "$BIN" ] || [ ! -x "$BIN" ]; then
  if [ -n "${CC_LOADOUT_LAUNCHER_NO_DOWNLOAD:-}" ]; then
    echo "cc-loadout launcher: $BIN is not available and downloads are disabled" >&2
    exit 1
  fi
  download
  gc_old_versions
fi

reconcile_link

if [ "$print_path_only" -eq 1 ]; then
  printf '%s\n' "$BIN"
  exit 0
fi
exec "$BIN" "$@"
