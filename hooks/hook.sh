#!/bin/bash
# Plugin-owned hook shim, shared by SessionStart and SessionEnd.
#
# This carries no logic beyond resolving the binary and handing stdin over.
# Resolution is delegated entirely to scripts/launcher.sh, which downloads the
# version pinned in .claude-plugin/cli-version — so the skills this plugin
# ships and the binary they drive are always the same build.
#
# $1 is the binary's own hook subcommand name (session-start | session-end).
#
# Every path exits 0. A hook that fails must degrade silently, never block a
# session from starting. Note that stdout here is injected into the session's
# context, so anything printed to it is user-facing: diagnostics belong on
# stderr, and hints are deliberate.

event="${1:-}"
[ -n "$event" ] || exit 0

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
launcher="$here/../scripts/launcher.sh"

# The predecessor of this block looked the binary up on PATH, and needed a
# careful `-f` plus `-x` dance to reject a directory, a non-executable file and
# a shell function that `command -v` would all have reported as usable. None of
# that applies now: the launcher resolves one exact path that it created itself,
# and refuses a non-executable there rather than exec'ing it. The retired tests
# for those cases live on in tests/test_launcher.sh.
if [ "$event" = "session-end" ]; then
  # A session ending is not the moment to spend thirty seconds downloading. If
  # the binary was never provisioned, there are no on-demand holds to release.
  bin="$(CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 sh "$launcher" --print-path 2>/dev/null)" || bin=""
  [ -n "$bin" ] || exit 0
else
  err_file="$(mktemp)"
  bin="$(sh "$launcher" --print-path 2>"$err_file")" || bin=""
  if [ -z "$bin" ]; then
    # Plugin installed, CLI unavailable. The bundled skills all drive the CLI,
    # so without it they fail at preflight with no explanation. Say so once, at
    # session start only. Point, don't scold — and pass the launcher's own
    # reason through rather than guessing at it.
    cat <<EOF
cc-loadout: could not provision its CLI, so its skills can't run.
$(sed 's/^/  /' "$err_file")
If this persists, build from source and set CC_LOADOUT_BIN to the binary.
EOF
    rm -f "$err_file"
    exit 0
  fi
  rm -f "$err_file"
fi

# A standalone install predating the plugin-managed layout keeps working, but
# it is a different build from the one the skills now drive — which makes every
# bug report ambiguous about which version ran. Name it once, at session start.
#
# [ -L ] first, and cheaply: the overwhelmingly common case is our own symlink,
# which must cost nothing. Only a real file justifies spawning a subprocess.
# Skipped entirely under CC_LOADOUT_BIN, so developers running their own build
# are not nagged about it.
if [ "$event" = "session-start" ] && [ -z "${CC_LOADOUT_BIN:-}" ]; then
  link="${CC_LOADOUT_LINK_DIR:-$HOME/.local/bin}/cc-loadout"
  if [ ! -L "$link" ] && [ -f "$link" ] && [ -x "$link" ]; then
    pin="$(tr -d '[:space:]' <"$here/../.claude-plugin/cli-version" 2>/dev/null || true)"
    # clap's `--version` prints "cc-loadout X.Y.Z"; take the last field of the
    # first line rather than assuming the whole format. An empty result (the
    # binary is too old for --version, or broken) means no hint: this is a
    # courtesy, not a diagnostic worth guessing at.
    have="$("$link" --version 2>/dev/null | head -1 | awk '{print $NF}')"
    if [ -n "$have" ] && [ -n "$pin" ] && [ "$have" != "$pin" ]; then
      cat <<EOF
cc-loadout: $link is a standalone install ($have); the plugin manages $pin.
Converge them with:
  $bin doctor --fix
EOF
    fi
  fi
fi

# Not `|| true`: a non-zero exit used to mean "the installed CLI is older than
# this plugin", which the version pin has made impossible — the plugin always
# runs its own build. So a failure here is a genuine bug, and the binary's own
# stderr is the diagnostic. Swallow the status (the hook must exit 0) without
# inventing an explanation for it on stdout.
"$bin" hook "$event" || :
exit 0
