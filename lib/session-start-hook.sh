#!/bin/bash
# Installed as the cc-loadout SessionStart hook (see
# lib/registry.sh::install_session_hook). Reads the hook's stdin JSON, exports
# CC_LOADOUT_SESSION_ID for the rest of this session (so `cc-loadout profile
# on-demand acquire` can find it), then re-runs scope promotion so
# universal/on_demand plugins stay resolvable across repos.
set -euo pipefail

lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
input="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$input" 2>/dev/null || true)"

if [[ -n "$session_id" && -n "${CLAUDE_ENV_FILE:-}" ]]; then
  echo "export CC_LOADOUT_SESSION_ID=$session_id" >> "$CLAUDE_ENV_FILE"
fi

# shellcheck source=lib/registry.sh
source "$lib_dir/registry.sh"
promote_universal_to_user >/dev/null 2>&1 || true
promote_on_demand_to_user >/dev/null 2>&1 || true
promote_profiles_to_user >/dev/null 2>&1 || true
