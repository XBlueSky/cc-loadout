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
  #
  # `</dev/null` on every subprocess this shim spawns (here and below): the
  # SessionStart/SessionEnd JSON payload is still unread on our own stdin at
  # this point, and without this a spawned process that happens to read stdin
  # (the launcher itself does not, but the resolved binary is whatever is
  # actually installed at the PATH link — see the standalone-install check
  # further down) would consume it, leaving nothing for `"$bin" hook
  # "$event"` below to parse.
  bin="$(CC_LOADOUT_LAUNCHER_NO_DOWNLOAD=1 sh "$launcher" --print-path 2>/dev/null </dev/null)" || bin=""
  [ -n "$bin" ] || exit 0
else
  # mktemp itself can fail (an unwritable or full TMPDIR, say). Unguarded,
  # `err_file` would end up empty, `2>"$err_file"` immediately below would
  # become the invalid redirect `2>""`, and ITS failure — not the launcher's
  # — would make `bin` come back empty: a perfectly healthy CLI reported as
  # unprovisionable. Fall back to resolving without a scratch file to capture
  # stderr into; the only cost is losing the first-run "downloading ..."
  # hint in this already-rare case.
  err_file="$(mktemp 2>/dev/null)" || err_file=""
  if [ -n "$err_file" ]; then
    bin="$(sh "$launcher" --print-path 2>"$err_file" </dev/null)" || bin=""
  else
    bin="$(sh "$launcher" --print-path 2>/dev/null </dev/null)" || bin=""
  fi
  if [ -z "$bin" ]; then
    # Plugin installed, CLI unavailable. The bundled skills all drive the CLI,
    # so without it they fail at preflight with no explanation. Say so once, at
    # session start only. Point, don't scold — and pass the launcher's own
    # reason through rather than guessing at it (when we managed to capture
    # one at all; see the mktemp guard above).
    {
      echo "cc-loadout: could not provision its CLI, so its skills can't run."
      [ -z "$err_file" ] || sed 's/^/  /' "$err_file"
      echo "If this persists, build from source and set CC_LOADOUT_BIN to the binary."
    }
    [ -z "$err_file" ] || rm -f "$err_file"
    exit 0
  fi
  # On success $err_file still holds whatever the launcher wrote to its own
  # stderr — on a first run, exactly one line ("downloading <asset> (v<pin>)")
  # that exists so a human waiting on that download learns why the session is
  # taking longer than usual. Discarding it unconditionally (as this used to)
  # makes that line unreachable in the one case it was written for. `-s`
  # keeps a warm start (nothing written, launcher returned instantly) silent
  # on both streams — this must never become an unconditional `cat`. stdout
  # stays untouched either way; this goes to stderr and nowhere else.
  if [ -n "$err_file" ] && [ -s "$err_file" ]; then
    cat "$err_file" >&2
  fi
  [ -z "$err_file" ] || rm -f "$err_file"
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
  # Normalized the same way, and for the identical reason, as
  # scripts/launcher.sh's own CC_LOADOUT_LINK_DIR handling (see that script's
  # normalize_dir_var comment): a trailing slash or a relative value here must
  # resolve to the SAME directory the launcher and doctor.rs agree on, or this
  # check ends up probing a path none of them actually maintain.
  link_dir_env="${CC_LOADOUT_LINK_DIR:-}"
  case "$link_dir_env" in
  /*) ;;
  *) link_dir_env="" ;;
  esac
  while [ -n "$link_dir_env" ] && [ "${link_dir_env%/}" != "$link_dir_env" ]; do
    link_dir_env="${link_dir_env%/}"
  done
  link="${link_dir_env:-$HOME/.local/bin}/cc-loadout"
  if [ ! -L "$link" ] && [ -f "$link" ] && [ -x "$link" ]; then
    pin="$(tr -d '[:space:]' <"$here/../.claude-plugin/cli-version" 2>/dev/null || true)"
    # clap's `--version` prints "cc-loadout X.Y.Z"; take the last field of the
    # first line rather than assuming the whole format. Purely descriptive
    # now: this used to gate the hint below on `have != pin`, but version
    # equality does not imply build equality — a hand-built 0.1.23 is not the
    # released 0.1.23 — so a file whose reported version happened to match
    # the pin was never named, even though it is still a different build
    # from the one the skills drive. False assurance in exactly the case
    # this check exists to catch. Every regular file at the link path is
    # named now, regardless of what (or whether) it reports.
    # `</dev/null`: same stdin-isolation reasoning as the launcher spawns
    # above — whatever is actually installed here is the user's own file,
    # and it must not be handed the still-unread session JSON.
    have="$("$link" --version 2>/dev/null </dev/null | head -1 | awk '{print $NF}')"
    cat <<EOF
cc-loadout: $link is a standalone install${have:+ ($have)}; the plugin manages $pin.
Converge them with:
  $bin doctor --fix
EOF
  fi
fi

# Not `|| true`: a non-zero exit used to mean "the installed CLI is older than
# this plugin", which the version pin has made impossible — the plugin always
# runs its own build. So a failure here is a genuine bug, and the binary's own
# stderr is the diagnostic. Swallow the status (the hook must exit 0) without
# inventing an explanation for it on stdout.
"$bin" hook "$event" || :
exit 0
