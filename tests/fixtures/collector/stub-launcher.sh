#!/bin/bash
# Test stand-in for `node` in tests/collector.rs: cc-loadout runs it as
#   node --import tsx <repo>/src/launcher/main.ts run --config <plan> \
#        --attempt-ref <attempt> --session-id <session>
# It never runs node, Geralt's launcher or claude. The file after `--config`
# is a plan the test wrote: bash `KEY=VALUE` lines that say how to behave.
#
#   DUMP_DIR                 where to record what this run saw (required)
#   EXIT_CODE                exit code of a natural end (default 0)
#   OUTCOME                  one whole line printed on stdout at the end
#   STDOUT_BYTES             this many extra bytes printed on stdout first
#   STDERR_LINES             lines printed on stderr, separated by `|`
#   SLEEP_SECS               how long to run before the natural end (default 0)
#   IGNORE_TERM=1            ignore SIGTERM
#   ON_TERM_EXIT             exit code on SIGTERM (default 30)
#   ON_TERM_SLEEP_SECS       how long to keep running after SIGTERM first
#   GRANDCHILD               none | plain | setsid | escape (default none);
#                            escape: a middle layer starts a setsid grandchild
#                            and ends at once
#   GRANDCHILD_IGNORE_TERM=1 the grandchild ignores SIGTERM
#   GRANDCHILD_SLEEP_SECS    how long the grandchild runs (default 60)
#   GRANDCHILD_HOLDS_STDOUT=1 the grandchild keeps this run's stdout/stderr
#   GRANDCHILD_DONE_FILE     file the grandchild creates when it ends by itself
#
# Dumped to $DUMP_DIR/run-<pid>/: argv (NUL-separated), environ, stdin
# (readlink of fd 0), pgid, pwd, then `ready` once everything below is set up.
# Every process this script starts (itself, its sleeps, the grandchild and its
# sleeps) appends `<pid> <starttime>` to $DUMP_DIR/pids before it can be
# signalled by the test, so the test can clean up exactly these and no other.
set -u
PATH=/usr/bin:/bin

# `<pid> <starttime>` of process $1 (field 22 of /proc/<pid>/stat, counted
# after the parenthesised command name).
report() {
  local line rest
  read -r line < "/proc/$1/stat" || return 0
  rest=${line##*) }
  # shellcheck disable=SC2086
  set -- "$1" $rest
  echo "$1 ${21}" >> "$DUMP_DIR/pids"
}

# Sleep in a child that is reported first, so the test can always kill it; a
# trapped signal still interrupts the wait.
nap() {
  sleep "$1" &
  local p=$!
  report "$p"
  wait "$p"
}

defaults() {
  : "${DUMP_DIR:?DUMP_DIR missing from the plan}"
  EXIT_CODE=${EXIT_CODE:-0}
  OUTCOME=${OUTCOME:-}
  STDOUT_BYTES=${STDOUT_BYTES:-0}
  STDERR_LINES=${STDERR_LINES:-}
  SLEEP_SECS=${SLEEP_SECS:-0}
  IGNORE_TERM=${IGNORE_TERM:-}
  ON_TERM_EXIT=${ON_TERM_EXIT:-30}
  ON_TERM_SLEEP_SECS=${ON_TERM_SLEEP_SECS:-}
  GRANDCHILD=${GRANDCHILD:-none}
  GRANDCHILD_IGNORE_TERM=${GRANDCHILD_IGNORE_TERM:-}
  GRANDCHILD_SLEEP_SECS=${GRANDCHILD_SLEEP_SECS:-60}
  GRANDCHILD_HOLDS_STDOUT=${GRANDCHILD_HOLDS_STDOUT:-}
  GRANDCHILD_DONE_FILE=${GRANDCHILD_DONE_FILE:-}
}

# The grandchild: `stub-launcher.sh __grandchild <plan> <run dir>`.
if [ "${1:-}" = __grandchild ]; then
  # shellcheck source=/dev/null
  . "$2"
  defaults
  if [ "$GRANDCHILD_IGNORE_TERM" = 1 ]; then trap '' TERM; fi
  report "$$"
  : > "$3/gc-ready"
  nap "$GRANDCHILD_SLEEP_SECS"
  if [ -n "$GRANDCHILD_DONE_FILE" ]; then : > "$GRANDCHILD_DONE_FILE"; fi
  exit 0
fi

PLAN=
prev=
for a in "$@"; do
  if [ "$prev" = --config ]; then PLAN=$a; fi
  prev=$a
done
if [ -z "$PLAN" ]; then
  echo "stub-launcher: no --config" >&2
  exit 97
fi
# shellcheck source=/dev/null
. "$PLAN"
defaults

on_term() {
  if [ -n "$ON_TERM_SLEEP_SECS" ]; then nap "$ON_TERM_SLEEP_SECS"; fi
  exit "$ON_TERM_EXIT"
}
if [ "$IGNORE_TERM" = 1 ]; then trap '' TERM; else trap on_term TERM; fi

report "$$"
D=$DUMP_DIR/run-$$
mkdir -p "$D"
printf '%s\0' "$@" > "$D/argv"
cat "/proc/$$/environ" > "$D/environ"
readlink "/proc/$$/fd/0" > "$D/stdin"
ps -o pgid= "$$" | tr -d ' ' > "$D/pgid"
pwd -P > "$D/pwd"

gc() {
  if [ "$GRANDCHILD_HOLDS_STDOUT" = 1 ]; then
    "$@" &
  else
    "$@" > /dev/null 2>&1 &
  fi
}
case "$GRANDCHILD" in
  none) ;;
  plain) gc "$BASH" "$0" __grandchild "$PLAN" "$D" ;;
  setsid) gc setsid "$BASH" "$0" __grandchild "$PLAN" "$D" ;;
  escape) ( gc setsid "$BASH" "$0" __grandchild "$PLAN" "$D" ) ;;
  *) echo "stub-launcher: bad GRANDCHILD" >&2; exit 98 ;;
esac
if [ "$GRANDCHILD" != none ]; then
  i=0
  while [ ! -e "$D/gc-ready" ] && [ "$i" -lt 500 ]; do
    sleep 0.02
    i=$((i + 1))
  done
fi
: > "$D/ready"

if [ -n "$STDERR_LINES" ]; then
  IFS='|' read -r -a lines <<< "$STDERR_LINES"
  for l in "${lines[@]}"; do printf '%s\n' "$l" >&2; done
fi
if [ "$STDOUT_BYTES" -gt 0 ]; then
  head -c "$STDOUT_BYTES" /dev/zero | tr '\0' x
fi
if [ "$SLEEP_SECS" != 0 ]; then nap "$SLEEP_SECS"; fi
if [ -n "$OUTCOME" ]; then printf '%s\n' "$OUTCOME"; fi
exit "$EXIT_CODE"
