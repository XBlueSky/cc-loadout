# cc-loadout

> Claude Code loadout manager — switch between Claude accounts, and build + apply the right plugin profile for each repo.

[![CI](https://github.com/xbluesky/cc-loadout/actions/workflows/ci.yml/badge.svg)](https://github.com/xbluesky/cc-loadout/actions/workflows/ci.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Contributor Covenant](https://img.shields.io/badge/Contributor%20Covenant-2.1-purple.svg)](CODE_OF_CONDUCT.md)

**English** | [繁體中文](README.zh-TW.md)

![cc-loadout demo — hub TUI tour](docs/assets/demo.gif)

`cc-loadout` is a small Rust CLI that manages two parts of your Claude Code setup:

- **Accounts** — snapshot the credentials of each Claude Max / subscription account and switch between them with one command (e.g. when one account hits its session limit).
- **Profiles** — decide which plugins each repo needs, and keep that in sync. An interactive **board** (`cc-loadout profile init`, or just run `cc-loadout` and tab to Profile) scans your installed plugins and repos so you can sort them into profiles — optionally letting Claude draft the grouping for you — and writes your `profiles.json`. Open any profile to edit its **detection rules** (which repos it applies to) right in the board, with a live count of matching repos and near-miss hints as you type. It then turns the profile-specific plugins **off globally** (keeping the universal ones on), so by default a repo loads only the universal set; `apply` re-enables each repo's matching plugins in its `.claude/settings.local.json`. The result: the right plugins per repo, instead of loading all of them everywhere.

## Quickstart

```bash
# 1. Install (from a clone; needs a Rust toolchain)
git clone https://github.com/xbluesky/cc-loadout && cd cc-loadout && ./install.sh

# 2. Snapshot the account you're logged into — then your others
cc-loadout account add work
#    /login to another account in Claude Code, then:
cc-loadout account add personal

# 3. Switch in seconds when one account hits its 5-hour limit
cc-loadout account use personal --launch

# 4. Set up per-repo plugin profiles — open the hub:
cc-loadout
#    → Tab to Profile. On first run it offers "✨ Let Claude draft your
#      profiles?" (or sort the Unassigned plugins yourself), then press w
#      to apply. Each repo now loads only the plugins that fit it.
```

New here? The Profile tab is a **board**: every plugin sits under **Universal**
(loaded everywhere), a **profile** (loaded in matching repos), or **Unassigned**.
Setup = empty the Unassigned bucket, then `w` to apply.

## Why

- **Session limits.** With more than one Claude subscription, hitting a limit on one account shouldn't stop your work — `cc-loadout account use <other>` swaps the active login in seconds.
- **Plugin budget.** Claude Code loads every enabled plugin's skills into every session, overflowing the skill-description budget and mis-firing skills across repo types. `cc-loadout` disables the profile-specific plugins globally and re-enables each only in the repos that match its profile, so a session loads just the universal plugins plus the few that fit the current repo.

## Features

- Single binary, no runtime dependencies.
- `account` — snapshot / switch / list real Claude logins; transactional, atomic credential swaps with rollback and post-switch verification.
- `profile inventory` — list your installed plugins and per-repo signals (also `--json`, to feed the wizard and the agent skill).
- `profile init` / `edit` — an interactive **board** (also reachable by running `cc-loadout` and tabbing to Profile) that builds `profiles.json` by sorting installed plugins into profiles, with an optional `✨` AI draft (Claude proposes the grouping) and a re-edit view that flags drift (new/uninstalled plugins, uncovered repos, global drift). Atomic write; backs up any existing file; adjusts the global enabled set so non-universal plugins stop loading everywhere. `profile init --root <dir> --assign <file>` runs the same setup non-interactively for agents / CI.
- **Edit detection rules in the board** — open a profile's Detail view → Rules tab to author what it matches, no JSON by hand: four rule kinds (`path under` / `has file` / `has any` / `contains`), a live match-count and **near-miss** panel as you type, `?` to explain why any scanned repo matches (or doesn't), `f` to seed rules from an example repo, and ghost path-completion for `path under` values.
- `profile detect` / `apply` — per-repo plugin detection (path prefix, marker files, marker globs, and file-content matches — a named file containing a word; legacy `package.json`-deps / dependency-keyword rules are still honoured for older configs) with manual override; additive universal + profile plugin sets; a surgical merge that preserves your on-demand and unrelated settings. `--all` sweeps every git repo under your scan roots.
- Ships as a Claude Code plugin too: the bundled `/cc-loadout:init` skill creates your profiles by chatting with Claude (no TTY needed); the board's `✨` AI draft is the in-TUI equivalent.

## Install

### From source (requires a Rust toolchain)

```bash
git clone https://github.com/xbluesky/cc-loadout ~/code/cc-loadout
cd ~/code/cc-loadout
./install.sh
```

`install.sh`:

- builds the release binary with `cargo` and copies it to `~/.local/bin/cc-loadout`;
- seeds `~/.claude/profiles/profiles.json` from `profiles.example.json` (only if absent — your edits are safe);
- promotes universal plugins to `scope: user` and installs a SessionStart hook to keep them that way.

It is idempotent — re-run after pulling a new version, or whenever plugin registry scope drifts. Your `profiles.json` is never overwritten.

### Pre-built (once a release is published)

```bash
curl -sSL https://raw.githubusercontent.com/xbluesky/cc-loadout/master/install.sh | bash
```

Make sure `~/.local/bin` is on your `PATH`.

### As a Claude Code plugin (guided setup)

cc-loadout also ships as a Claude Code plugin bundling a guided profile-creation skill.
Add it as a marketplace, then install:

```
/plugin marketplace add https://github.com/xbluesky/cc-loadout
/plugin install cc-loadout@cc-loadout
```

Then run `/cc-loadout:init` — or just ask Claude to "set up my cc-loadout profiles". The
skill drives the `cc-loadout` CLI, so install the binary too (above). The interactive
`cc-loadout profile init` TUI is the no-agent alternative.

## Usage

### Accounts

```bash
cc-loadout account add work                 # snapshot the currently logged-in account as "work"
# /login to your other account in Claude Code, then:
cc-loadout account add personal
cc-loadout account list                     # '*' marks the active account; shows email / org / token status
cc-loadout account use work                 # swap credentials only (restart Claude / `claude --continue` to apply)
cc-loadout account use work --launch        # swap, then relaunch `claude --continue`
cc-loadout account current
cc-loadout account rm personal
cc-loadout account prime personal           # anchor 'personal's 5h window now (--json for machine output)
cc-loadout account schedule                  # interactive wizard (human)
cc-loadout account schedule set personal 06:00 11:00 16:00   # non-interactive (AI): set times
cc-loadout account schedule clear personal   # clear one account's schedule (omit alias = clear all)
cc-loadout account schedule list --json      # schedule + next_fire (RFC3339) + last_primed
cc-loadout account list --json               # machine-readable list (also: current --json, status --json)
```

> **Interactive (no alias).** `cc-loadout account` (or `account use` with no alias) opens a picker to switch accounts; `account prime` / `account rm` with no alias pick interactively too. Pass an explicit `<alias>` for the non-interactive (scriptable / agent) form.

Snapshots live in `~/.local/share/cc-loadout/accounts/<alias>/` (credential files are `0600`). A switch is transactional: it reads the target first, refreshes the outgoing snapshot, writes the new login atomically (rolling back on failure), then verifies the live account actually changed.

**Window priming.** Claude's 5-hour usage window starts on an account's first request, not on the wall clock. `account prime <alias>` sends a minimal request *as that account* (isolated, without disturbing the active one) so its window opens at a time you choose; `account schedule` writes those times into a managed `cron` block that runs `account prime` for you. Priming the currently active account is a no-op (it would rotate the live session's token out from under it). cron is the only scheduler — there is no daemon.

**JSON output for agents.** Read commands take `--json` (`account list`/`current`, `account schedule list`, `account prime`, and top-level `status`). Output is a single object `{ "schema_version": 1, … }` on stdout with exit 0; errors exit non-zero with a human message on stderr. New keys may be added without bumping `schema_version`, so consumers should ignore unknown keys. `status --json` also includes a `priming` section (per scheduled account: `next_fire`, `last_primed`).

### Profiles

```bash
# inside a git repo:
cc-loadout profile inventory          # list installed plugins + per-repo signals
cc-loadout profile inventory --json   # same, machine-readable (used by the agent flow)
cc-loadout profile inventory --root /path/to/tree   # scan one tree instead of all scan_roots
cc-loadout profile init               # interactive board -> profiles.json (+ adjusts the global plugin set)
cc-loadout profile init --root <dir> --assign <file|-> --json   # non-interactive (agents/CI) — see "Headless setup"
cc-loadout profile edit               # interactively edit an existing profiles.json
cc-loadout profile detect            # which profiles match + the resulting plugin set
cc-loadout profile apply             # write enabledPlugins to .claude/settings.local.json
cc-loadout profile status            # what's currently enabled
cc-loadout profile detect --json     # machine-readable: repos[].{matched,plugins,signals[{profile,rule,value}]}
cc-loadout profile apply  --json     # applies + reports the enabledPlugins diff as JSON
cc-loadout profile status --json     # machine-readable: repos[].applied = enabled plugin keys
cc-loadout profile force frontend    # pin this repo to specific profile(s) via .claude/profile

# across every git repo under scan_roots:
cc-loadout profile detect --all
cc-loadout profile apply  --all
cc-loadout profile apply  --all --dry-run   # audit only: which repos are out of sync (writes nothing)
```

`apply --all` prints only the repos it changed, then a summary line. Add
`--dry-run` to answer "which repos still need applying?" without writing —
useful after cloning new repos into a scan root:

```console
$ cc-loadout profile apply --all --dry-run
--- /src/new-repo ---
Profiles: rust
  rust-analyzer-lsp@claude-plugins-official: (unset) -> true

Summary: 1 of 996 repos would change; 24 match no profile.
```

Pair it with `--json` to script the audit — `repos[].changed` is empty for
every repo that is already in sync:

```bash
cc-loadout profile apply --all --dry-run --json \
  | jq -r '.repos[] | select(.changed|length>0) | .repo'
```

### Editing detection rules in the TUI

`cc-loadout profile edit` (or run `cc-loadout`, tab to **Profile**, press `v` for
the by-profile view, and open a profile) lands on the board. Opening a profile
reaches its **Detail** view; `Tab` to the **Rules** tab to author what the
profile matches — no JSON by hand. Four rule kinds cover every case:

| Rule | Matches a repo when… |
|---|---|
| `path under` | the repo lives under a folder (path prefix) |
| `has file` | a file with that exact name sits at the repo root |
| `has any` | any file matches a glob (`*.vue`, `*.rs`) |
| `contains` | a named file contains a word (e.g. `requirements.txt` → `torch`) |

As you build a rule the board shows a **live count** of matching repos and a
**near-miss** panel — repos that *almost* match, with the one rule that would
catch them. Keys in the Rules tab: `a` add, `e` edit, `d` delete, `f` seed rules
from an example repo, `?` explain why a chosen repo matches (or doesn't). A
`path under` value offers ghost directory-completion — `→` accepts it. Repos that
pin themselves with a `.claude/profile` override are left out of the preview,
since detect rules don't classify them.

### Headless setup (agents / CI)

`profile init` opens the interactive TUI by default. Pass `--assign` to run it
non-interactively, with no terminal — this is the path an AI agent or a script
uses. The full setup is three steps:

1. **Inspect.** `cc-loadout profile inventory --root <dir> --json` reports the
   installed plugins (`plugins[].key`) and the profiles it suggests from the
   scan (`suggested_profiles[].name`, each carrying the detect markers it found).

2. **Assign + write.** Map the installed plugins onto `universal` (loaded in
   every repo) and onto the suggested profile names, then write it:

   ```bash
   cat > assignment.json <<'JSON'
   {
     "universal": ["serena@official"],
     "profiles": {
       "rust": ["rust-analyzer@community"],
       "node": ["eslint@community"]
     }
   }
   JSON
   cc-loadout profile init --root <dir> --assign assignment.json --json
   ```

   This writes `profiles.json` (each profile's detect rules come from the scan)
   and adjusts the **global** enabled set (`~/.claude/settings.json`): universal
   plugins stay enabled, every other managed plugin is disabled there, so it no
   longer loads in every repo. Input is validated strictly: an unknown profile
   name, an uninstalled plugin key, an unknown JSON field, or an empty
   assignment aborts before anything is written. `--assign -` reads stdin; the
   `--json` result includes a `next_step` reminder.

3. **Activate per repo.** `cc-loadout profile apply --all` enables each repo's
   matching plugins in its `.claude/settings.local.json`, bringing a
   profile-specific plugin back on only in the repos it matches.

Net effect: every repo loads just the universal plugins by default, and each
profile's plugins appear only in the repos that match it.

## Configuration

Profiles live in `~/.claude/profiles/profiles.json` (override the path with `$CC_LOADOUT_PROFILES`). The easiest way to create it is `cc-loadout profile init` (or the `/cc-loadout:init` skill), which scans your repos and walks you through the choices. To hand-write it instead, start from the shipped `profiles.example.json`. Universal plugins are enabled in every repo; profiles add on top and stack. Writing `profiles.json` via `profile init` also adjusts the global enabled set in `~/.claude/settings.json` (universal plugins stay on, other managed plugins are turned off there) so they stop loading in every repo; `apply` then re-enables each repo's matching plugins locally.

Each profile's `detect` block supports `path_prefixes`, `marker_files`, `marker_globs`, and `content` (a list of `{ "file": …, "word": … }` pairs — a file that contains a word). These are exactly the four rule kinds the Rules tab edits (`path under` / `has file` / `has any` / `contains`). Older configs may also carry `package_json_deps` / `deps_keywords`; they still match, and the TUI shows them read-only so you can rewrite them as `content` rules.

The example below is a **starting point, not a recommendation** — the four profiles are there to demonstrate the four kinds of detection rule, not because you need those particular plugins. Point `scan_roots` at the absolute paths where you keep your repos (note: `~` is **not** expanded — use full paths), then replace the profiles with whatever plugin groupings fit your work. The engine has no built-in knowledge of any plugin, language, or framework; every name below comes from this JSON and nowhere else.

| Profile | Adds | Detected by | Rule kind |
|---|---|---|---|
| `frontend` | ui-ux-pro-max, impeccable, frontend-design | `*.vue` anywhere, or `package.json` containing vue/react/svelte | `marker_globs` + `content` |
| `plugin-dev` | plugin-dev, skill-creator | `.claude-plugin/marketplace.json` or `plugin.json` at repo root | `marker_files` |
| `ai-side` | rag, prompt-engineering | `requirements.txt` / `pyproject.toml` containing langchain/openai/anthropic/llamaindex | `content` |
| `work` | *(swap in your own)* | any repo under `/home/you/work/` | `path_prefixes` |

To override detection, write profile names (one per line) to `.claude/profile`, or run `cc-loadout profile force <name>…`. `apply` writes only to `.claude/settings.local.json` (gitignored) and preserves any keys it does not manage.

## Notes & limitations

- **Linux-first.** Credentials are read/written as files (`~/.claude/.credentials.json` and the `oauthAccount` block of `~/.claude.json`); `$CLAUDE_CONFIG_DIR` is honoured. macOS Keychain is not yet supported.
- These are Claude Code's **internal, undocumented** files and may change between releases. A switch verifies the result and fails loudly rather than silently leaving you on the wrong account.
- `account use` swaps credentials only by default; pass `--launch` to relaunch Claude (`claude --continue`). A running Claude reads the login into memory at startup, so it won't pick up the new account until it is restarted.

## Development

```bash
cargo build
cargo test                                              # unit + integration
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
./tests/run.sh                                          # bash installer/registry tests (needs bash, jq, git)
demo/record.sh                                          # re-render the README demo GIF (needs vhs, ttyd, ffmpeg)
```

CI (`.github/workflows/ci.yml`) runs the same checks. `release.yml` publishes four targets as `.tar.gz` + `.sha256` Release assets — `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (both fully static), `x86_64-apple-darwin`, `aarch64-apple-darwin` — and `release-plz` derives the version and changelog from commit types. The target list and asset layout match [cc-uplink](https://github.com/xbluesky/cc-uplink), so the same `uname` → target mapping resolves either project's binaries.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, the check gate, and the pull-request flow. Participation is governed by our [Code of Conduct](CODE_OF_CONDUCT.md). To report a security issue, follow [SECURITY.md](SECURITY.md) — please don't open a public issue.

## License

[MIT](LICENSE) © XBlueSky
