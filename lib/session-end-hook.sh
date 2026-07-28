#!/bin/bash
# Installed as the cc-loadout SessionEnd hook (see
# lib/registry.sh::install_session_end_hook). Releases every on-demand hold
# this session took out in the repo the session ran in.
set -euo pipefail

input="$(cat)"
session_id="$(jq -r '.session_id // empty' <<<"$input" 2>/dev/null || true)"
cwd="$(jq -r '.cwd // empty' <<<"$input" 2>/dev/null || true)"

if [[ -z "$session_id" || -z "$cwd" ]]; then
  exit 0
fi

if ! command -v cc-loadout >/dev/null 2>&1; then
  exit 0
fi

(cd "$cwd" && cc-loadout profile on-demand release --session-id "$session_id" --all) \
  >/dev/null 2>&1 || true
