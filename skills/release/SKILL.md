---
name: release
description: >-
  Manually release a Profiles.on_demand plugin this session acquired (before
  the session ends), or force-clear a stuck/crash-orphaned on-demand hold in
  the current repo. Use when the user says "release X", "I'm done with X",
  "turn off the on-demand plugin", or invokes `/cc-loadout:release`. Normally
  this happens automatically on session end — this skill is for the early or
  crash-recovery case.
---

# Release an on-demand plugin

## Prerequisite

The `cc-loadout` CLI must be installed (`cc-loadout --version`). If it is
missing, tell the user to install it and stop.

## Steps

1. **Resolve which plugin.** If the user named one, use it directly. If not,
   list current holdings:
   ```
   cc-loadout profile on-demand list --json
   ```
   and ask which `key` (among those with `live_holders > 0`) to release.

2. **Release it:**
   ```
   cc-loadout profile on-demand release <key>
   ```
   This releases only the CURRENT session's hold — if another session still
   holds the same plugin, it stays enabled for them (this is by design; holds
   are shared, not exclusive).

3. **If the user is trying to clear a stuck hold from a crashed session**
   (they mention a plugin that's still on despite no session using it, or ask
   to "force" release), confirm with them first — this reverts it regardless
   of any other session that might still hold it — then run:
   ```
   cc-loadout profile on-demand release <key> --force
   ```

## Boundaries

- **Don't `--force` without confirmation.** It reverts a hold regardless of
  other sessions still using it — always confirm with the user first (Step
  3), never fire it just because a plugin "looks" stuck.
- **Don't release plugins the user isn't done with.** Only release keys the
  user names or confirms from the `live_holders > 0` list — not every
  on-demand plugin the repo has ever acquired.
