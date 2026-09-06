# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

## [0.1.28] - 2026-09-04

### Bug Fixes

- stop promising "⏎ open" on the empty Unassigned row

### Features

- open Universal and On-demand from the profile board

### Refactoring

- remove the in-TUI AI draft

## [0.1.27] - 2026-09-04

### Bug Fixes

- close the on-demand plugin leak at global scope

## [0.1.26] - 2026-08-31

### Features

- prune redundant scope:local plugin records

## [0.1.25] - 2026-08-28

### Bug Fixes

- drop enabledPlugins keys that leave the managed set

## [0.1.24] - 2026-08-14

### Bug Fixes

- normalize the data dir, order reconcile before GC, harden the launcher's failure surface
- install.sh must not follow a symlink onto the pinned release binary
- spell the converged symlink through the literal data root
- address doctor --fix convergence review findings
- surface the launcher's stderr on a successful SessionStart
- make GC's rm -rf fail-soft and refresh a stale launcher comment
- guard bare command substitutions in the launcher test suite

### Documentation

- correct plugin-managed-CLI claims across READMEs, CONTRIBUTING, SECURITY and skills

### Features

- install into the plugin's data dir layout
- converge a standalone install in doctor --fix
- resolve the CLI through the launcher in the hook shim
- maintain the PATH symlink and prune superseded versions
- download and verify the pinned binary in the launcher
- add the plugin launcher's binary resolution
- pin the CLI version for the plugin launcher

## [0.1.23] - 2026-08-04

### Bug Fixes

- bump scan cache version so caches from the old walk are rebuilt
- find repos whose .git is a symlink or file, prune tooling trees

## [0.1.22] - 2026-08-04

### Features

- let scheduled tasks pick a model, force primes onto haiku

## [0.1.21] - 2026-08-03

### Bug Fixes

- document Cloudflare Pages Node version
- add canonical site metadata
- use representative reduced-motion demo
- use public registry lockfiles
- stabilize copy control announcements
- clarify compact brand and copy controls
- respect reduced motion for TUI demo
- improve page landmarks and copy feedback
- ignore generated Astro site artifacts

### CI/CD

- validate and build cc-loadout website

### Documentation

- add logo to readmes

### Features

- publish site metadata and manifest
- add cinematic scroll storytelling
- build cc-loadout product story
- scaffold manifest-driven Astro site
- add marketplace presentation data

### Miscellaneous

- ignore local node modules
- ignore local worktrees

## [0.1.20] - 2026-07-31

### Bug Fixes

- honest pending cue while typing an unindexed rule
- explain overlay survives a repo vanishing mid-scan
- refresh in-memory inventory after commit
- recover indexing flag when the IndexAtoms worker dies
- clear uncovered_pending after rescan; correct stale doc comments
- canonicalize repo path at scan time for path_prefix parity

### Features

- index preview + fresh-write divergence report
- v1 scan-cache migration via banner + background rebuild
- v2 index scan with budget reporting
- detached background indexing for new rule atoms
- index-backed rules tab with honest pending states
- tri-state signal evaluator mirroring detect_one
- index rule atoms + override names per repo
- multi-glob single walk with a dirent budget
- RepoSignal rule_hits/override_names + scan-cache v2 marker
- rule atoms + index vocabulary for signal detect

### Refactoring

- explain overlay reads the index
- drift from signals; retire RecomputeUncovered job

### Testing

- zero-walk regression net + profile docs

## [0.1.19] - 2026-07-30

### Bug Fixes

- keep a hooks-less group instead of dropping it as empty
- reclaim profiles.json backups, stop claiming health it never checked
- error on an unknown flag instead of falling through to a real install
- stop swallowing a too-old binary's non-zero exit in hook.sh
- make cc-loadout's own key unconditionally managed
- require -f alongside -x when resolving cc-loadout
- treat a non-executable cc-loadout on PATH as missing
- stop swallowing corrupt profiles.json/registry, fix stale-backup matching
- sanitize session_id in env-file write, add session_end coverage
- tighten anchor to close residual collision risks
- remove unused PathBuf import
- anchor legacy command matching and add settings.json backup
- error handling in registry promotion and performance tweaks

### Documentation

- add doctor to the README feature lists

### Features

- surface plugin scope drift on the profile board
- ship SessionStart/SessionEnd hooks with the plugin
- add cc-loadout doctor for install inspection and repair
- add cc-loadout hook session-start/session-end
- remove retired settings.json hook entries
- port registry scope promotion to Rust

### Refactoring

- retire lib/ shell hooks in favour of plugin-owned hooks
- bootstrap via the binary, stop installing hooks

### Testing

- neutralise CC_LOADOUT_PROFILES and CLAUDE_ENV_FILE in cmd()
- prove scope drift survives the Snapshot->Drift assembly wiring
- cover a command-less entry surviving legacy-hook removal
- isolate the --help/--version probe too
- isolate --print-mode assertions unconditionally

## [0.1.18] - 2026-07-28

### CI/CD

- align release targets and asset layout with cc-uplink
- sync plugin.json version onto the release PR
- bump actions/checkout to v5

## [0.1.17] - 2026-07-28

### Features

- add apply --dry-run to audit out-of-sync repos

## [0.1.16] - 2026-07-23

### Bug Fixes

- drop on-demand membership when a plugin is reassigned via the picker

### Features

- explain the on-demand bucket with a ? help overlay
- add a quiet help cue to the on-demand board row
- label on-demand plugins in the by-plugin view

### Style

- tighten on_demand_help::render visibility and refresh membership docstring

## [0.1.15] - 2026-07-17

### Features

- skip Raw recording on internal claude -p calls

### Perf

- drop needless uncovered recomputes and auto-backfill legacy caches
- cache uncovered drift so startup skips the per-repo walk

## [0.1.14] - 2026-07-15

### Bug Fixes

- autosave profiles.json on every edit (no more lost rules)
- live-commit Detail edits into working; Esc no longer discards

### Features

- cache scan results across sessions with staleness warning
- add backup-free atomic writer for autosave

## [0.1.13] - 2026-07-08

### Bug Fixes

- promote profile-tier plugins to scope: user

## [0.1.12] - 2026-07-07

### Bug Fixes

- prime the active account instead of skipping it

## [0.1.11] - 2026-07-06

### Testing

- stop unit + CLI tests from wiping the real user crontab

## [0.1.10] - 2026-07-03

### Bug Fixes

- box ActivePoll::Done's draft field to satisfy large_enum_variant
- migrate old inline SessionStart hook on upgrade
- lock apply() against on-demand writes; correct spec's crash-mitigation wording
- no-op cleanly on malformed stdin in session hooks
- lock on_demand state read-modify-write against concurrent sessions
- exclude on_demand plugins from the unassigned/drift pool

### Documentation

- add /cc-loadout:acquire and /cc-loadout:release

### Features

- add On-demand target option to the Assign flow
- render On-demand row on the profile Board
- install SessionEnd hook to auto-release on-demand holds
- SessionStart exports session_id and promotes on_demand scope
- add profile on-demand acquire/release/list subcommands
- add on_demand acquire/release/release_all core logic
- add on_demand field to Profiles schema

### Miscellaneous

- production-readiness cleanup — lock scoping, missing tests, on_demand preservation, docs

### Refactoring

- generalize scope promotion, add promote_on_demand_to_user

### Testing

- isolate schedule/status assertions from the real system crontab
- cover no-command-field and old+new-coexist hook migration cases

## [0.1.9] - 2026-07-02

### Bug Fixes

- install and verify the crontab before persisting the schedule

## [0.1.8] - 2026-07-02

### Bug Fixes

- align Accounts columns and give org its own detail row
- scroll long lists into view

### Perf

- background the post-edit drift recompute so rule edits don't freeze
- run repo scans on the job thread with a spinner
- prune build dirs from detect glob walk

## [0.1.7] - 2026-06-30

### Bug Fixes

- align Tasks columns and de-key-ify the resume hint
- validate task ids, guard prime/task kind, record last_primed on scheduled runs

### Features

- open the Tasks tab via bare `cc-loadout task` and show scheduled times
- wire Tasks tab run/remove actions through the job thread
- add Tasks view with run/delete key actions
- add the schedule skill for guided task setup
- back account schedule presets and status with tasks.json
- back the TUI schedule editor with tasks.json
- wire the task CLI subcommands
- add task resume entry point
- generate the managed crontab block from tasks.json
- orchestrate task run with location, profile and creds sync
- add shared headless claude runner
- seed isolated per-account config dir with plugins
- add run-location selection
- parameterise cron splice markers and render task lines
- add tasks.json model and validation

### Refactoring

- retire schedule.rs and the old prime cron path

## [0.1.6] - 2026-06-24

### Bug Fixes

- engine-attributed explain, override filter in explain, edit double-count
- Tab during repo picker no longer steals Detail focus
- builder owns the keyboard so Rules-tab keys reach the view
- guard Tab-toggle so contains-rule builder can switch file→word field
- marker_files no longer short-circuits on a content-referenced file
- Overview footer no longer advertises unhandled nav keys
- let a view claim single-letter keys; revive Detail r-rename
- offer line no longer truncates; drop sparkle emoji
- drive continuous redraws during by-plugin desc reveal
- sync plugin.json version to 0.1.5 (match Cargo.toml)
- /cc-loadout:init drives init --assign + apply --all (Model C)
- reap zombie + surface stderr on draft_with_claude failure
- drain stdout concurrently to prevent pipe deadlock
- degrade gracefully on malformed profiles.json; esc discards detail edits
- center content in a max-width column; right-align header clock
- prevent panic at small heights, handle detached jobs
- don't let global q/r/R shortcuts swallow text-field input
- refresh only on successful schedule write; restore PATH in test
- construct Action via apply_action so variants are live

### CI/CD

- fetch tags + full history for release-plz

### Documentation

- document the in-TUI detection-rule editor
- add Quickstart, refresh Profile to the board/AI flow, add 繁體中文
- sync the top-level Profiles/Why summary to Model C
- document the headless agent profile setup flow + global plugin behavior
- note window_start_epoch is a proxy anchor, not authoritative

### Features

- ghost path completion in the 'path under' rule builder
- explain (?) — why a repo matches a profile
- prefill detection rules from an example repo (f)
- live match count in the rule builder while typing
- wire Rules tab into Detail — [Plugins] [Rules]
- RulesState add/edit builder with live preview
- RulesState navigation + delete
- RulesState — render rule list + live match/near-miss preview
- rules.rs — live match preview + near-miss suggestions
- rules.rs — flatten/add/remove Detect as human RuleRows
- author derives content pairs from package.json deps
- detect engine matches content rules
- add ContentRule + Detect.content schema
- view-first key routing via claims_key; fix shadowed sub-view keys
- ghost path completion when adding/editing a scan root
- Scan Roots manager — 'r' adds/edits/removes roots
- track multiple scan roots; 's' scans their union
- 'r' edits the scan root in the by-plugin view
- AI draft scans first when repos are unscanned
- App::new defers the repo scan; seeds suggested scan root
- by-plugin header shows scan root + s scan; by-profile footer gains s scan
- 's' triggers the explicit repo scan on the Board
- ProfileView holds scan_root + explicit scan()
- add build_inventory_no_scan (plugins only, no repo walk)
- by-plugin multi-profile membership picker
- by-plugin view — all plugins + descriptions + v toggle
- scan_draft no longer pre-assigns plugins (empty profile buckets)
- read plugin descriptions from marketplace.json
- consent-gated AI draft offer + 'a' key on the board
- async job can return an AI draft to the active view
- ai draft — claude proposes plugin→profile assignment
- render drift badges on the profile board
- Snapshot carries the global enabled-plugins set
- drift — uncovered repos + Drift aggregate
- drift — stale references + global-scope drift detection
- apply sub-view — repo multiselect, dry-run expand, commit
- per-profile detail — plugins, example-repo detection, rename, delete
- assign unassigned plugins to profiles
- Commit action replaces WriteProfiles (adds repo apply)
- commit() writes profiles + global + selected repos
- derive detection from example repos; list unassigned plugins
- scan_draft seeds an editable config from inventory
- strict assignment parse + apply --all reminder in non-interactive init
- profile init --assign for non-interactive (AI/headless) setup
- non-interactive init_noninteractive (scan + assign -> profiles.json + apply_global)
- the wizard write also applies the global plugin state
- annotate currently-global plugins in the Universal picker
- show global enabledPlugins before→after in the wizard review
- apply_global — set global enabledPlugins (universal on, profile-specific off)
- redesign the hub — inline panel + Claude-Code UX across all tabs
- run switch/prime/remove/write as background jobs with a spinner
- adaptive tick + ambient motion (frame counter, breathing, toast fade)
- restyle Schedule + Profile wizard + widgets to the warm palette
- restyle Overview + Accounts to the warm palette (borderless)
- Direction-A chrome — header, accent tab rule, contextual footer
- warm Claude palette tokens (aliases keep views compiling)
- deep-link profile init/edit into the hub Profile wizard
- write profiles.json from the Profile wizard preview
- profile wizard per-profile loop + end-to-end test
- profile wizard skeleton — scan-root to universal step
- hand-rolled MultiSelect checkbox widget
- deep-link bare account schedule into the hub Schedule tab
- write schedule.json and regenerate crontab from the Schedule tab
- editable Schedule tab (nav, toggle, inline HH:MM edit)
- hand-rolled single-line TextInput widget
- deep-link no-alias account commands into the hub Accounts tab
- relaunch claude --continue, restoring terminal before exec
- confirm modal before account removal
- switch/prime/remove actions wired through apply_action
- navigable Accounts list; drop Snapshot.active double-source
- launch hub on bare cc-loadout; status snapshot when non-tty
- App shell with tab strip and key dispatch
- View trait, live Overview, placeholder tabs
- add theme and live 5h-window gauge
- gather hub snapshot from the domain layer
- add ratatui dep and tui module skeleton

### Miscellaneous

- drop now-redundant dead_code allows in init.rs
- drop redundant dead_code allows + dedup; tidy test
- warn that profile init --root is ignored; fix stale comment
- allow forward-looking ctx/snapshot fields used by later plans

### Refactoring

- remove description-pane reveal animation
- dedup NEW_PROFILE sentinel and improve picker borrow
- extract pub(crate) validate + assemble_profiles for reuse
- consolidate test helpers; fix detail render; add const
- split profile tab into a board module + navigation
- wire PANEL into modal+spinner; drop legacy theme aliases
- retire inquire prompts; headless hint for non-TTY interactive cmds
- add schedule_ops::write seam; TUI uses it (no PATH-mut test)
- drop unused MultiSelect::is_empty (YAGNI, was dead-coded)
- thread now_local into View::render for live time math

### Testing

- pin override-exclusion and marker/content partial-overlap gate
- strengthen content_does_not_hijack_marker_files
- assert global enable/disable direction in profile init test
- non-tty profile init keeps the inquire fallback
- non-tty bare account schedule keeps the inquire fallback
- non-tty bare account keeps the inquire fallback

### Polish

- board path display, cursor-follows-edit, handler dedup

### Style

- scan bar above the title, centered, accent keys

### Features

- redesign the hub as a fixed-height inline panel with a Claude-Code aesthetic across all four tabs (Overview / Accounts / Schedule / Profile): focus shown as a background bar, two-tone gauges, aligned rows
- profile hybrid activation: `profile init` now also adjusts the global enabled set (`~/.claude/settings.json`) so non-universal plugins stop loading in every repo, then `apply` re-enables each repo's plugins locally
- non-interactive `profile init --root <dir> --assign <file|->` for agents / CI: scans, validates strictly (unknown profile/plugin/field or empty assignment aborts before any write), writes `profiles.json` + the global set; `--json` output carries a `next_step` reminder

### Documentation

- document the headless agent setup flow (`inventory --json` -> `init --assign` -> `apply --all`) and the global vs per-repo plugin behavior in the README

## [0.1.5] - 2026-06-17

### Bug Fixes

- retry ETXTBSY on spawn and bound claude -p with a timeout
- dedupe applied-keys via enabled_keys; clarify diff null doc

### Documentation

- document --json and non-interactive schedule

### Features

- interactive switch/prime/rm pickers (no-alias TUI)
- add Priming section (next-fire + last-primed)
- show next-fire and last-primed in schedule list
- add time dep and next_fire computation
- surface detect signals in --json and human output
- capture detect provenance (rule + matched value)
- put inventory --json on the shared envelope; docs
- add --json to detect, apply, status
- add typed JSON payloads + enabled_keys helper
- non-interactive schedule set/clear/list CLI
- add --json combined view
- add --json outcome to account prime
- add --json to account list and current

### Refactoring

- hoist exec_claude_continue into util for reuse

### Testing

- inject binaries in tests to eliminate PATH-mutation flakiness
- cover schedule clear-all path

## [0.1.4] - 2026-06-16

### Documentation

- align slot command grammar and help text under the loadout model
- link community files from README
- add code of conduct and security policy
- add contributing guide

### Features

- preflight claude on PATH before --launch relaunch
- add state.json schema version with fail-fast on newer files
- add read-only top-level status combined view

### Miscellaneous

- add issue and pull request templates
- sync plugin.json version with Cargo.toml

### Refactoring

- extract slot-agnostic merge_object JSON primitive

### Testing

- add orthogonality invariant regression gate for the two slots
- route all integration tests through the isolating cmd() helper

## [0.1.3] - 2026-06-16

### Bug Fixes

- inventory works before profiles.json exists
- harden init/edit wizard and profiles.json backup
- keep frontend cluster signals in sync; warn on missing registry

### Documentation

- surface guided profile creation across the README
- apply skill-creator guidance to /cc-loadout:init skill
- document Claude Code plugin install + /cc-loadout:init
- document profile inventory command
- clarify discover suggestion semantics

### Features

- add /cc-loadout:init guided-creation skill
- package cc-loadout as a Claude Code plugin
- add interactive init/edit subcommands
- interactive init/edit TUI shell
- pure profiles.json authoring + atomic write
- make config serializable; add inquire dep
- add inventory subcommand
- assemble inventory and resolve registry path
- cluster repos into suggested profiles
- extract per-repo signals in discover

### Testing

- guard bundled skill presence; clarify verify caveat

## [0.1.2] - 2026-06-12

### Features

- don't relaunch by default; add --launch flag

## [0.1.1] - 2026-06-12

### Bug Fixes

- stop reporting usable tokens as expired

### Features

- show remaining time for valid tokens in list
- warn when adding a duplicate account
# Changelog

All notable changes to this project will be documented in this file.
