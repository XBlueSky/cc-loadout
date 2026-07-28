# shellcheck source=helpers.sh
source "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../lib/registry.sh
source "$ROOT/lib/registry.sh"

fakehome="$(mktemp -d)"
mkdir -p "$fakehome/.claude/plugins"
registry="$fakehome/.claude/plugins/installed_plugins.json"
profiles="$fakehome/profiles.json"

# --- promote_on_demand_to_user promotes a local+projectPath entry to user ---
cat > "$registry" <<'JSON'
{
  "plugins": {
    "pixijs@x": [
      {"scope": "local", "projectPath": "/some/other/repo", "lastUpdated": 1}
    ]
  }
}
JSON
cat > "$profiles" <<'JSON'
{"scan_roots":[],"universal":[],"profiles":{},"on_demand":["pixijs@x"]}
JSON
CC_LOADOUT_PROFILES="$profiles" promote_on_demand_to_user "$registry" >/dev/null
scope="$(jq -r '.plugins["pixijs@x"][0].scope' "$registry")"
assert_eq "$scope" "user" "promote_on_demand_to_user sets scope: user"
has_project_path="$(jq -r '.plugins["pixijs@x"][0] | has("projectPath")' "$registry")"
assert_eq "$has_project_path" "false" "promote_on_demand_to_user strips projectPath"

# --- promote_universal_to_user still works through the generalized helper ---
cat > "$registry" <<'JSON'
{
  "plugins": {
    "univ@x": [
      {"scope": "local", "projectPath": "/some/other/repo", "lastUpdated": 1}
    ]
  }
}
JSON
cat > "$profiles" <<'JSON'
{"scan_roots":[],"universal":["univ@x"],"profiles":{},"on_demand":[]}
JSON
CC_LOADOUT_PROFILES="$profiles" promote_universal_to_user "$registry" >/dev/null
scope2="$(jq -r '.plugins["univ@x"][0].scope' "$registry")"
assert_eq "$scope2" "user" "promote_universal_to_user still works through the generic helper"

# --- promote_profiles_to_user promotes plugins listed under any profile ---
# Regression: profile-specific plugins were never promoted, so a plugin
# installed at scope: local (bound to its original repo) reported "not cached"
# in every other repo where a matching profile enabled it.
cat > "$registry" <<'JSON'
{
  "plugins": {
    "prof-a@x": [
      {"scope": "local", "projectPath": "/some/other/repo", "lastUpdated": 1}
    ],
    "prof-b@y": [
      {"scope": "local", "projectPath": "/some/other/repo", "lastUpdated": 1}
    ]
  }
}
JSON
cat > "$profiles" <<'JSON'
{"scan_roots":[],"universal":[],"on_demand":[],"profiles":{
  "one":{"plugins":["prof-a@x"],"detect":{"marker_files":["Cargo.toml"]}},
  "two":{"plugins":["prof-b@y"]}
}}
JSON
CC_LOADOUT_PROFILES="$profiles" promote_profiles_to_user "$registry" >/dev/null
scope_a="$(jq -r '.plugins["prof-a@x"][0].scope' "$registry")"
assert_eq "$scope_a" "user" "promote_profiles_to_user promotes a plugin from the first profile"
scope_b="$(jq -r '.plugins["prof-b@y"][0].scope' "$registry")"
assert_eq "$scope_b" "user" "promote_profiles_to_user promotes a plugin from a second profile"
pp_a="$(jq -r '.plugins["prof-a@x"][0] | has("projectPath")' "$registry")"
assert_eq "$pp_a" "false" "promote_profiles_to_user strips projectPath"

# --- install_session_hook migrates a pre-existing old-style inline hook ---
settings="$fakehome/.claude/settings.json"
mkdir -p "$fakehome/.claude"
cat > "$settings" <<'JSON'
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash -c 'source \"/some/old/path/registry.sh\" && promote_universal_to_user >/dev/null 2>&1' || true"}]}]}}
JSON
install_session_hook "$settings" >/dev/null
count="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count" "1" "install_session_hook leaves exactly one SessionStart entry after migrating the old form"
new_cmd_present="$(jq -r '[.hooks.SessionStart[]?.hooks[]?.command] | any(contains("session-start-hook.sh"))' "$settings")"
assert_eq "$new_cmd_present" "true" "install_session_hook installs the new-form command during migration"
old_cmd_present="$(jq -r '[.hooks.SessionStart[]?.hooks[]?.command] | any(contains("promote_universal_to_user"))' "$settings")"
assert_eq "$old_cmd_present" "false" "install_session_hook removes the old-form command during migration"
rm -f "$settings"

# --- install_session_hook does not touch an unrelated plugin's SessionStart hook ---
cat > "$settings" <<'JSON'
{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bash /some/other/plugin/hook.sh"}]}]}}
JSON
install_session_hook "$settings" >/dev/null
count="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count" "2" "install_session_hook adds its own entry alongside an unrelated plugin's hook (2 total)"
unrelated_present="$(jq -r '[.hooks.SessionStart[]?.hooks[]?.command] | any(. == "bash /some/other/plugin/hook.sh")' "$settings")"
assert_eq "$unrelated_present" "true" "install_session_hook leaves the unrelated plugin's hook command unchanged"
rm -f "$settings"

# --- install_session_hook is idempotent once the new form is installed ---
install_session_hook "$settings" >/dev/null
install_session_hook "$settings" >/dev/null
count="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count" "1" "install_session_hook run twice with the new form installed results in exactly one entry"
rm -f "$settings"

# --- install_session_hook preserves hook entries lacking .command field ---
cat > "$settings" <<'JSON'
{"hooks":{"SessionStart":[{"hooks":[{"type":"prompt","prompt":"some text"}]}]}}
JSON
install_session_hook "$settings" >/dev/null
count="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count" "2" "install_session_hook adds new entry alongside hook without .command field (2 total)"
prompt_hook_present="$(jq -r '[.hooks.SessionStart[]?.hooks[]? | select(.type == "prompt")] | length' "$settings")"
assert_eq "$prompt_hook_present" "1" "install_session_hook preserves the prompt hook without .command field"
prompt_hook_unchanged="$(jq -r '.hooks.SessionStart[0].hooks[0]' "$settings")"
prompt_value="$(echo "$prompt_hook_unchanged" | jq -r '.prompt')"
assert_eq "$prompt_value" "some text" "install_session_hook leaves the prompt hook's prompt field unchanged"
new_cmd_present="$(jq '[.hooks.SessionStart[]?.hooks[]?.command | select(. != null)] | any(contains("session-start-hook.sh"))' "$settings")"
assert_eq "$new_cmd_present" "true" "install_session_hook adds the new cc-loadout entry alongside no-command hook"
rm -f "$settings"

# --- install_session_hook removes old form when both old and new coexist ---
# First, create initial empty state, then install new form hook
cat > "$settings" <<'JSON'
{}
JSON
install_session_hook "$settings" >/dev/null
# Verify new form was installed
count="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count" "1" "install_session_hook creates exactly one entry initially"
# Now manually add the old-form hook back to simulate a coexisting state
jq '.hooks.SessionStart += [{"hooks":[{"type":"command","command":"bash -c source registry.sh promote_universal_to_user"}]}]' "$settings" > "$settings.tmp" && mv "$settings.tmp" "$settings"
# Now we have both old and new forms
count_before="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count_before" "2" "fixture setup: both old and new forms present before migration"
# Call install_session_hook again to test the migration when both coexist
install_session_hook "$settings" >/dev/null
count_after="$(jq '[.hooks.SessionStart[]?.hooks[]?] | length' "$settings")"
assert_eq "$count_after" "1" "install_session_hook removes old form when new form already exists (1 total)"
# Check that the old-form hook (containing registry.sh) is gone
old_cmd_gone="$(jq '[.hooks.SessionStart[]?.hooks[]?.command] | map(select(. != null)) | any(contains("registry.sh"))' "$settings")"
assert_eq "$old_cmd_gone" "false" "install_session_hook removes the old-form command when new form coexists"
# Check that the new-form hook is still there
new_cmd_present="$(jq '[.hooks.SessionStart[]?.hooks[]?.command] | map(select(. != null)) | any(contains("session-start-hook.sh"))' "$settings")"
assert_eq "$new_cmd_present" "true" "install_session_hook preserves the new-form command"
rm -f "$settings"

rm -rf "$fakehome"
