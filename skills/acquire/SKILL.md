---
name: acquire
description: >-
  Temporarily enable a Profiles.on_demand plugin in the current repo for this
  Claude Code session only — reverted automatically when the session ends.
  Use when the user wants to use a plugin they've deliberately kept out of
  universal/profiles (e.g. pixijs-skills, a vendor-specific MCP) just for this
  task, says "acquire X", "borrow X for now", "turn on X for this session",
  or invokes `/cc-loadout:acquire` with or without a plugin name. Requires the
  cc-loadout CLI and its SessionStart hook to have already run this session.
---

# Acquire an on-demand plugin for this session

You enable a `Profiles.on_demand` plugin in the current repo, scoped to this
session, via the `cc-loadout` CLI. You do not edit `.claude/settings.local.json`
or the on-demand state file yourself — the CLI does both.

## Prerequisite

The `cc-loadout` CLI must be installed (`cc-loadout --version`). If it is
missing, tell the user to install it and stop.

## Steps

1. **Resolve which plugin.** If the user named one (e.g. `/cc-loadout:acquire
   pixijs-skills`), match it against the on_demand list:
   ```
   cc-loadout profile on-demand list --json
   ```
   Fuzzy-match the given name against the returned `key`s (e.g. `pixijs-skills`
   matching `pixijs-skills@some-marketplace`). If there's no match, show the
   full list and ask which one they meant — do not guess silently.

   If the user gave no name, run the same `list --json` command, present every
   `key` (and its `live_holders` count if non-zero) to the user, and ask which
   one to acquire.

2. **Acquire it:**
   ```
   cc-loadout profile on-demand acquire <key>
   ```
   If this fails with a `CC_LOADOUT_SESSION_ID not set` error, tell the user
   the cc-loadout SessionStart hook hasn't run in this session (likely not
   installed — point them at `install.sh`) and stop.

3. **Tell the user to reload.** The plugin is now enabled in
   `.claude/settings.local.json`, but Claude Code doesn't pick up newly
   enabled plugin components until you run:
   ```
   /reload-plugins
   ```
   Tell the user to run it now themselves — you cannot invoke a slash command
   on their behalf.

4. **Remind them it's session-scoped.** It will revert automatically when this
   session ends (via the cc-loadout SessionEnd hook). If they want to release
   it early, point them at `/cc-loadout:release`.

## Guardrails

- **Let the CLI own state.** Do not edit `.claude/settings.local.json` or the
  on-demand state file directly yourself — `cc-loadout profile on-demand
  acquire` does both.
- **Only `on_demand` plugins are acquirable.** A plugin not returned by
  `profile on-demand list` cannot be acquired this way — it needs to be added
  to `Profiles.on_demand` first (via `/cc-loadout:init`'s Assign flow).
- **Never invoke `/reload-plugins` on the user's behalf.** Tell them to run it
  themselves; no hook can fire a slash command for them.
