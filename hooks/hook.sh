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
# `command -v` reports a PATH match without checking that it is executable,
# while the fallback below uses `-x`. Left inconsistent, a non-executable
# cc-loadout on PATH (partial install, a copy that dropped the exec bit) is
# treated as "found": the shim runs it, bash emits a raw "Permission denied",
# and the install hint — the one message that exists to explain a broken
# install — never prints, precisely when the install is broken. An unusable
# binary must be indistinguishable from a missing one.
#
# `-f` is required alongside `-x` at the $HOME/.local/bin fallback site below:
# `-x` alone returns true for a directory too (the traversal bit is set by
# default), so a stray directory named cc-loadout there would otherwise be
# "found" and exec'd, failing the same way with "Is a directory" instead of
# "Permission denied". This is not redundant — do not simplify it back to a
# bare `-x`.
#
# That directory hazard cannot occur at THIS site, though: bash's PATH search
# never hands `command -v` a directory (measured — a directory placed on PATH
# yields no match at all). `-f` is still required here, for a different
# reason: `command -v` also succeeds for a shell function or alias, printing
# a bare name with no filesystem meaning behind it (measured — a defined
# `cc-loadout` shell function makes `command -v cc-loadout` print the bare
# name, rc=0). `-f` is what rejects that name instead of treating it as usable.
#
# Deliberately out of scope: an executable regular file containing garbage
# (mode 755, invalid program) still reaches `"$bin" hook "$event"` below and
# fails with "Exec format error". No filesystem check can distinguish a
# valid program from an invalid one without running it, the failure is
# already contained (exit 0, stderr only, no stdout pollution), and
# swallowing the shell's message would hide a real diagnostic along with it.
if [ -n "$bin" ] && { [ ! -f "$bin" ] || [ ! -x "$bin" ]; }; then
  bin=""
fi
if [ -z "$bin" ] && [ -f "$HOME/.local/bin/cc-loadout" ] && [ -x "$HOME/.local/bin/cc-loadout" ]; then
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

if ! "$bin" hook "$event"; then
  # Non-zero from `hook` means the binary did not understand the subcommand:
  # every fallible step inside session_start/session_end is deliberately
  # swallowed (see src/hooks/mod.rs), so a current binary always exits 0 here.
  # Almost always an installed CLI older than this plugin. Same tone as the
  # missing-binary hint above: point, don't scold; session-start only.
  if [ "$event" = "session-start" ]; then
    cat <<'EOF'
cc-loadout: the installed CLI is too old for this plugin, so scope upkeep is off. Update it with:
curl -sSL https://raw.githubusercontent.com/xbluesky/cc-loadout/master/install.sh | bash
EOF
  fi
fi
exit 0
