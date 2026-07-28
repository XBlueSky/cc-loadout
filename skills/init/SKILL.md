---
name: init
description: >-
  Guided creation of a cc-loadout profiles.json — enumerate the user's installed
  Claude Code plugins, scan their repos, and set up a per-repo plugin loadout for
  them via the cc-loadout CLI. Use this whenever a cc-loadout user wants to set up,
  create, bootstrap, or redo their profiles, decide which plugins load in which
  repos, or fix "too many plugins firing in every repo" — even if they don't say
  "cc-loadout" or "/cc-loadout:init" by name. Requires the cc-loadout CLI (it does
  the scanning, validating, writing, and per-repo activation).
---

# Guided cc-loadout profile creation

You help the user build and activate `~/.claude/profiles/profiles.json` for
`cc-loadout` — the config that decides which Claude Code plugins load in which
repos. You drive the `cc-loadout` CLI; **you do not hand-write the JSON or touch
`settings.json` yourself** — the CLI validates and writes everything.

The end state (the "loadout" model): **universal** plugins stay enabled globally,
every other managed plugin is turned **off** globally, and each repo re-enables
just the plugins of the profiles it matches. That takes two CLI calls —
`profile init --assign` (writes the config + adjusts the global set) and
`profile apply --all` (enables per repo). Do both, or the change does not take effect.

## Prerequisite

The `cc-loadout` CLI must be installed (`cc-loadout --version`). If it is missing,
tell the user to install it (see the cc-loadout README) and stop.

## Steps

1. **Inspect.** Find out where the user keeps their git repos; ask for an absolute
   path if you don't already know it (cc-loadout does NOT expand `~` — use a full
   path). Then:
   ```
   cc-loadout profile inventory --json --root <that path>
   ```
   (Drop `--root` to use their existing `scan_roots`.) You get back
   `{ "schema_version": 1, "plugins": [...], "repos": [...], "suggested_profiles": [...] }`.
   Ignore `schema_version` and any unknown key. Read `plugins[].key`, `repos`, and
   `suggested_profiles[]` (each suggested profile has a `name` and the detect markers
   found in that cluster of repos).

2. **Interview.** Using the inventory, decide the assignment with the user:
   - Which installed plugins are **universal** (loaded in every repo)? Good universal
     picks never mis-fire anywhere (e.g. search / code-review tools).
   - For each entry in `suggested_profiles`, which of the remaining plugins should it
     add? Confirm or skip each suggestion. (Profile names come from
     `suggested_profiles[].name` — you cannot invent new profile names here; the
     detect rules are derived from the scan.)

3. **Assign + write (one command — do NOT hand-write `profiles.json`).** Compose an
   assignment object: keys under `profiles` are `suggested_profiles[].name` values;
   every plugin string is an exact `name@marketplace` from `plugins[]`.
   ```json
   {
     "universal": ["serena@official"],
     "profiles": {
       "frontend": ["eslint@community", "prettier@community"],
       "rust": ["rust-analyzer@community"]
     }
   }
   ```
   Pass it to the CLI (write it to a file, or pipe it via `--assign -`):
   ```
   cc-loadout profile init --root <that path> --assign assignment.json --json
   ```
   This **validates strictly** (an unknown profile name, an uninstalled plugin key, an
   unknown field, or an empty assignment aborts before anything is written), writes
   `profiles.json` (backing up any existing file automatically), and adjusts the
   **global** enabled set in `~/.claude/settings.json` (universal stays on; every other
   managed plugin is turned off). The `--json` result includes a `next_step` reminder.
   If it errors, fix the assignment and re-run — nothing was written.

4. **Activate per repo.**
   ```
   cc-loadout profile apply --all
   ```
   This enables each repo's matching plugins in its `.claude/settings.local.json`.
   After this, every repo loads just the universal plugins plus the few that fit it.

5. **Verify.** Run `cc-loadout profile detect --all` (or `profile status --json`) and
   confirm each repo resolves to the profile(s) the user intended. If a repo matches
   nothing it should, revisit that profile in the inventory and re-run from step 3.
   Show the user the final result.

## Guardrails

- **Let the CLI do the writing.** `profile init --assign` validates the input, backs
  up an existing `profiles.json`, writes the new one, and adjusts the global set — do
  not hand-assemble JSON onto disk or edit `settings.json` yourself, and do not make
  manual `.bak` copies (the CLI does).
- **Both calls are required.** `init --assign` sets up the config + the global disable;
  `apply --all` does the per-repo enable. Skipping `apply --all` leaves the user with a
  config that has not taken effect (plugins still not enabled per repo).
- Use only plugin keys taken verbatim from the inventory's `plugins[]`, and only
  profile names from `suggested_profiles[]`. The CLI rejects anything else — but
  getting it right the first time avoids a round-trip.
- This sets up config and enablement only — it does NOT install plugins (that is
  Claude Code's job).
- If the user would rather click than chat, point them at the interactive board:
  run `cc-loadout` and tab to the Profile tab (it has the same setup, plus an optional
  `✨` "let Claude draft it" button). The board's `w` does the equivalent of
  `init --assign` + `apply --all` in one step.
