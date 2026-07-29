#!/bin/bash
# Plugin-owned hook shim, shared by SessionStart and SessionEnd.
#
# This carries no logic beyond locating the binary and handing stdin over.
# ${CLAUDE_PLUGIN_ROOT} points at the plugin, not at the binary, and a hook
# shell is not guaranteed to have ~/.local/bin on PATH — so the lookup has to
# happen somewhere, and it happens here exactly once for both events rather
# than in two files that can drift apart.
#
# $1 is the binary's own hook subcommand name (session-start | session-end).
#
# Every path exits 0. A hook that fails must degrade silently, never block a
# session from starting.

event="${1:-}"
[ -n "$event" ] || exit 0

bin="$(command -v cc-loadout 2>/dev/null || true)"
if [ -z "$bin" ] && [ -x "$HOME/.local/bin/cc-loadout" ]; then
  bin="$HOME/.local/bin/cc-loadout"
fi

if [ -z "$bin" ]; then
  # Plugin installed, binary missing. The bundled skills all drive the CLI, so
  # without it they fail at preflight with no explanation. Say so once, at
  # session start only — repeating it as the session ends would be noise at the
  # moment the user can least act on it. Point, don't scold.
  if [ "$event" = "session-start" ]; then
    cat <<'EOF'
cc-loadout: CLI not installed, so its skills can't run. Install it with:
curl -sSL https://raw.githubusercontent.com/xbluesky/cc-loadout/master/install.sh | bash
EOF
  fi
  exit 0
fi

"$bin" hook "$event" || true
exit 0
