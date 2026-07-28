# shellcheck shell=bash

# Generic: for each key in `keys` (newline-separated on stdin via the caller's
# <<<), ensure a `scope: user` entry exists in `registry`. If a user-scope
# entry already exists, leave it. Otherwise pick the most-recently-updated
# local/project entry, strip `projectPath`, set `scope: user`, and replace the
# entries array with just that one. Shared cache dir under installPath makes
# this safe across repos.
promote_keys_to_user() {
  local registry="$1"
  local keys="$2"  # newline-separated

  [[ -f "$registry" ]] || { echo "registry not found: $registry" >&2; return 1; }
  [[ -z "$keys" ]] && return 0

  local changed=0
  local tmp; tmp="$(mktemp)"
  cp "$registry" "$tmp"

  while IFS= read -r plugin; do
    [[ -z "$plugin" ]] && continue
    local has_user
    has_user="$(jq --arg p "$plugin" '
      .plugins[$p] // [] | map(select(.scope == "user")) | length
    ' "$tmp")"
    if [[ "$has_user" -gt 0 ]]; then
      continue
    fi
    local has_any
    has_any="$(jq --arg p "$plugin" '.plugins[$p] // [] | length' "$tmp")"
    if [[ "$has_any" -eq 0 ]]; then
      echo "  skip $plugin (not installed)"
      continue
    fi
    jq --arg p "$plugin" '
      .plugins[$p] |= (
        (sort_by(.lastUpdated) | reverse | .[0])
        | del(.projectPath)
        | .scope = "user"
        | [.]
      )
    ' "$tmp" > "$tmp.new" && mv "$tmp.new" "$tmp"
    echo "  promoted $plugin -> scope: user"
    changed=1
  done <<<"$keys"

  if [[ "$changed" -eq 1 ]]; then
    cp "$registry" "$registry.bak.$(date +%s)"
    mv "$tmp" "$registry"
    echo "registry updated: $registry (backup written)"
  else
    rm -f "$tmp"
    echo "registry already consistent — no changes"
  fi
}

# Ensure every plugin in profiles.json `.universal[]` has a `scope: user`
# entry. Without this, Claude Code's session-start plugin loader can't
# resolve `enabledPlugins: true` for universal plugins outside the repo where
# they were first installed.
promote_universal_to_user() {
  local registry="${1:-$HOME/.claude/plugins/installed_plugins.json}"
  local profiles="${CC_LOADOUT_PROFILES:-$HOME/.claude/profiles/profiles.json}"
  [[ -f "$profiles" ]] || { echo "profiles not found: $profiles" >&2; return 1; }
  promote_keys_to_user "$registry" "$(jq -r '.universal[]' "$profiles")"
}

# Ensure every plugin in profiles.json `.on_demand[]` has a `scope: user`
# entry, so it can be `acquire`d from any repo, not just the one it was
# installed in.
promote_on_demand_to_user() {
  local registry="${1:-$HOME/.claude/plugins/installed_plugins.json}"
  local profiles="${CC_LOADOUT_PROFILES:-$HOME/.claude/profiles/profiles.json}"
  [[ -f "$profiles" ]] || { echo "profiles not found: $profiles" >&2; return 1; }
  promote_keys_to_user "$registry" "$(jq -r '.on_demand[]' "$profiles")"
}

# Ensure every plugin listed under any profile in profiles.json
# (`.profiles[].plugins[]`) has a `scope: user` entry. cc-loadout enables
# profile plugins per-repo via `enabledPlugins: true`, which Claude Code's
# session-start loader can only resolve when the plugin is user-scoped. A
# profile plugin left at `scope: local` (bound to the repo it was first
# installed in) reports "not cached" in every OTHER repo whose profile
# enables it — the same failure mode promote_universal_to_user fixes for the
# universal tier. Promoting all profile plugins (not just those matching the
# current repo) is safe and intentional: `scope: user` only means "resolvable
# from any repo"; whether a plugin is actually on is still governed per-repo
# by `enabledPlugins`.
promote_profiles_to_user() {
  local registry="${1:-$HOME/.claude/plugins/installed_plugins.json}"
  local profiles="${CC_LOADOUT_PROFILES:-$HOME/.claude/profiles/profiles.json}"
  [[ -f "$profiles" ]] || { echo "profiles not found: $profiles" >&2; return 1; }
  promote_keys_to_user "$registry" "$(jq -r '.profiles[]?.plugins[]?' "$profiles")"
}

# Install a SessionStart hook in ~/.claude/settings.json that runs
# lib/session-start-hook.sh every time Claude Code starts a session. That
# script exports CC_LOADOUT_SESSION_ID (so `cc-loadout profile on-demand
# acquire` can find the running session) and re-runs both
# promote_universal_to_user and promote_on_demand_to_user, so any plugin
# auto-update reverting universal/on_demand plugins to scope: local doesn't
# break the loader on the next session.
#
# Before checking idempotency and appending, this also migrates away any
# pre-existing inline hook installed by an older cc-loadout version (the
# `bash -c '... registry.sh ... promote_universal_to_user ...'` one-liner
# that predates session-start-hook.sh) so a machine upgrading from before
# this feature shipped ends up with exactly one cc-loadout SessionStart
# entry instead of a stale one plus the new one. Only command strings
# containing both "registry.sh" and "promote_universal_to_user" (and not
# already equal to the new command) are removed; every other plugin's
# SessionStart hooks are left completely untouched.
install_session_hook() {
  local settings="${1:-$HOME/.claude/settings.json}"
  local lib_dir; lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local cmd="bash \"$lib_dir/session-start-hook.sh\""

  [[ -f "$settings" ]] || echo '{}' > "$settings"

  local has_old
  has_old="$(jq --arg c "$cmd" '
    [.hooks.SessionStart[]?.hooks[]? | select(
      ((.command // "") | contains("registry.sh")) and
      ((.command // "") | contains("promote_universal_to_user")) and
      (.command != $c)
    )] | length
  ' "$settings")"

  local already
  already="$(jq --arg c "$cmd" '
    [.hooks.SessionStart[]?.hooks[]? | select(.command == $c)] | length
  ' "$settings")"

  if [[ "$has_old" -eq 0 && "$already" -gt 0 ]]; then
    echo "session hook already installed"
    return 0
  fi

  local tmp; tmp="$(mktemp)"
  cp "$settings" "$settings.bak.$(date +%s)"
  jq --arg c "$cmd" '
    (
      (.hooks.SessionStart // [])
      | map(.hooks = ((.hooks // []) | map(select(
          ((.command // "") | contains("registry.sh") | not)
          or ((.command // "") | contains("promote_universal_to_user") | not)
          or (.command == $c)
        ))))
      | map(select((.hooks | length) > 0))
    ) as $migrated
    | .hooks.SessionStart = (
        if any($migrated[]?.hooks[]?; .command == $c)
        then $migrated
        else $migrated + [{"hooks": [{"type": "command", "command": $c}]}]
        end
      )
  ' "$settings" > "$tmp" && mv "$tmp" "$settings"

  if [[ "$has_old" -gt 0 ]]; then
    echo "migrated old-style SessionStart hook and installed SessionStart hook in $settings"
  else
    echo "installed SessionStart hook in $settings"
  fi
}

# Install a SessionEnd hook in ~/.claude/settings.json that releases every
# on-demand hold the ending session took out (see lib/session-end-hook.sh).
install_session_end_hook() {
  local settings="${1:-$HOME/.claude/settings.json}"
  local lib_dir; lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  local cmd="bash \"$lib_dir/session-end-hook.sh\""

  [[ -f "$settings" ]] || echo '{}' > "$settings"

  local already
  already="$(jq --arg c "$cmd" '
    [.hooks.SessionEnd[]?.hooks[]? | select(.command == $c)] | length
  ' "$settings")"

  if [[ "$already" -gt 0 ]]; then
    echo "session end hook already installed"
    return 0
  fi

  local tmp; tmp="$(mktemp)"
  cp "$settings" "$settings.bak.$(date +%s)"
  jq --arg c "$cmd" '
    .hooks.SessionEnd = ((.hooks.SessionEnd // []) + [{
      "hooks": [{"type": "command", "command": $c}]
    }])
  ' "$settings" > "$tmp" && mv "$tmp" "$settings"
  echo "installed SessionEnd hook in $settings"
}
