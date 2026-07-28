# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- session-start-hook.sh exports CC_LOADOUT_SESSION_ID ---
env_file="$(mktemp)"
echo '{"session_id":"sess-xyz","hook_event_name":"SessionStart"}' \
  | CLAUDE_ENV_FILE="$env_file" bash "$ROOT/lib/session-start-hook.sh" >/dev/null 2>&1
env_body="$(cat "$env_file")"
assert_contains "$env_body" "export CC_LOADOUT_SESSION_ID=sess-xyz" \
  "session-start-hook.sh exports the session id"
rm -f "$env_file"

# --- session-start-hook.sh gracefully no-ops on malformed stdin ---
env_file="$(mktemp)"
echo "not json" | CLAUDE_ENV_FILE="$env_file" bash "$ROOT/lib/session-start-hook.sh" >/dev/null 2>&1
exit_code=$?
assert_eq "$exit_code" "0" \
  "session-start-hook.sh exits 0 on malformed JSON"
env_body="$(cat "$env_file")"
assert_eq "$env_body" "" \
  "session-start-hook.sh does not export anything on malformed JSON"
rm -f "$env_file"

# --- session-end-hook.sh releases everything the ending session held ---
CC_BIN="$ROOT/target/release/cc-loadout"
if [[ ! -x "$CC_BIN" ]]; then
  echo "  SKIP: target/release/cc-loadout not found — run 'cargo build --release' first"
  TEST_PASS=$((TEST_PASS+1))
else
  repo="$(make_repo)"
  hdir="$(mktemp -d)"
  profiles="$hdir/profiles.json"
  cat > "$profiles" <<'JSON'
{"scan_roots":[],"universal":[],"profiles":{},"on_demand":["pixijs@x"]}
JSON

  (
    cd "$repo"
    HOME="$hdir" CC_LOADOUT_PROFILES="$profiles" CC_LOADOUT_SESSION_ID="sess-end-test" \
      "$CC_BIN" profile on-demand acquire "pixijs@x" >/dev/null 2>&1
  )

  enabled_before="$(jq -r '.enabledPlugins["pixijs@x"]' "$repo/.claude/settings.local.json")"
  assert_eq "$enabled_before" "true" "acquire enabled pixijs@x before the SessionEnd hook runs"

  echo "{\"session_id\":\"sess-end-test\",\"cwd\":\"$repo\",\"hook_event_name\":\"SessionEnd\"}" \
    | PATH="$(dirname "$CC_BIN"):$PATH" bash "$ROOT/lib/session-end-hook.sh" >/dev/null 2>&1

  enabled_after="$(jq -r '.enabledPlugins["pixijs@x"] // false' "$repo/.claude/settings.local.json")"
  assert_eq "$enabled_after" "false" "session-end-hook.sh released pixijs@x on session end"

  cleanup_repo "$repo"
  rm -rf "$hdir"
fi

# --- session-end-hook.sh gracefully no-ops on malformed stdin ---
echo "not json" | bash "$ROOT/lib/session-end-hook.sh" >/dev/null 2>&1
exit_code=$?
assert_eq "$exit_code" "0" \
  "session-end-hook.sh exits 0 on malformed JSON"
