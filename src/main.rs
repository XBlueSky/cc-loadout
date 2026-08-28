//! cc-loadout — Claude Code loadout manager (account switching + plugin profiles).
#![deny(unsafe_code)]

mod account;
mod doctor;
mod hooks;
mod json;
mod profile;
mod status;
mod task;
mod tui;
mod util;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::account::paths::{self, ClaudePaths};
use crate::account::store::Store;

#[derive(Parser)]
#[command(
    name = "cc-loadout",
    version,
    about = "Claude Code loadout manager",
    long_about = "Manage your Claude Code loadout — the contents of independent slots.\n\
                  Each slot switches on its own and never touches the others:\n\
                  - account: which Claude login is active (machine-global)\n\
                  - profile: which plugins are enabled (per-repo)\n\
                  - status:  a read-only combined view of both"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// account slot — which Claude login is active (snapshot / switch; machine-global)
    Account {
        #[command(subcommand)]
        action: Option<AccountAction>,
    },
    /// profile slot — which plugins are enabled per repo (detect / apply / status / force)
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Read-only combined view of all loadout slots (active account + cwd profiles)
    Status {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// task slot — scheduled headless Claude runs (prime + real tasks; no subcommand = interactive TUI)
    Task {
        #[command(subcommand)]
        action: Option<TaskAction>,
    },
    /// Hook entry points for the bundled plugin's shims (not for direct use)
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Check and repair cc-loadout's own installation
    Doctor {
        /// Apply the repairs instead of only reporting them
        #[arg(long)]
        fix: bool,
        /// Also delete stale timestamped registry backups (requires --fix)
        #[arg(long = "prune-backups")]
        prune_backups: bool,
    },
}

#[derive(Subcommand)]
enum TaskAction {
    /// Create/replace a scheduled task
    Add {
        id: String,
        #[arg(long)]
        account: String,
        #[arg(long = "at", required = true, num_args = 1..)]
        times: Vec<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        profile: Option<String>,
        /// Model this run uses (alias or full name, e.g. haiku, claude-sonnet-4-6).
        /// Omit to inherit whatever the CLI resolves; primes default to haiku.
        #[arg(long, allow_hyphen_values = true)]
        model: Option<String>,
    },
    /// Remove a task
    Rm { id: String },
    /// List tasks + next fire time
    List {
        #[arg(long)]
        json: bool,
    },
    /// Run a task now (also the cron entry point)
    Run {
        id: String,
        #[arg(long)]
        quiet: bool,
    },
    /// Reopen the most recent session for a task
    Resume { id: String },
}

#[derive(Subcommand)]
enum HookAction {
    /// Publish the session id, re-assert plugin scope, finish legacy migration
    SessionStart,
    /// Release every on-demand hold this session took out
    SessionEnd,
}

#[derive(Subcommand)]
enum ProfileAction {
    /// Print matched profiles for the repo at <path> (default: cwd)
    Detect {
        path: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Detect + write .claude/settings.local.json
    Apply {
        path: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
        /// Report what would change without writing (audit which repos are out of sync)
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove named enabledPlugins keys from settings.local.json (cleanup for
    /// keys left behind before cc-loadout tracked what it manages)
    Prune {
        /// Plugin keys to remove, e.g. serena@claude-plugins-official
        keys: Vec<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        /// Report what would be removed without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Print currently-applied profiles
    Status {
        path: Option<PathBuf>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Write .claude/profile in cwd
    Force { names: Vec<String> },
    /// List installed plugins + per-repo signals (read-only)
    Inventory {
        /// Emit machine-readable JSON instead of a table
        #[arg(long)]
        json: bool,
        /// Inspect a single tree instead of every scan_roots entry
        #[arg(long)]
        root: Option<PathBuf>,
    },
    /// Create profiles.json. Interactive (TUI) by default; with --assign it runs
    /// non-interactively (for AI/scripts): scans --root and applies the assignment.
    Init {
        /// Scan a single tree instead of prompting for a scan root
        #[arg(long)]
        root: Option<PathBuf>,
        /// Non-interactive: JSON assignment file ('-' = stdin) of
        /// {"universal":[keys],"profiles":{"<suggested-name>":[keys]}}. Requires --root.
        #[arg(long)]
        assign: Option<PathBuf>,
        /// Emit machine-readable JSON (only meaningful with --assign).
        #[arg(long)]
        json: bool,
    },
    /// Interactively edit an existing profiles.json
    Edit,
    /// Session-scoped enable/disable for Profiles.on_demand plugins
    OnDemand {
        #[command(subcommand)]
        action: OnDemandAction,
    },
}

#[derive(Subcommand)]
enum OnDemandAction {
    /// List Profiles.on_demand keys with their current live-holder count
    List {
        #[arg(long)]
        json: bool,
    },
    /// Enable an on_demand plugin in this repo for the current session
    Acquire { key: String },
    /// Release this session's (or an explicit --session-id's) hold
    Release {
        /// Key to release. Omit with --all to release everything held.
        key: Option<String>,
        /// Explicit session id — used by the SessionEnd hook. Defaults to
        /// $CC_LOADOUT_SESSION_ID for interactive use.
        #[arg(long)]
        session_id: Option<String>,
        /// Release every key the session holds instead of a single <key>.
        #[arg(long)]
        all: bool,
        /// Revert regardless of remaining holders (crash-orphan escape hatch).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum AccountAction {
    /// Snapshot the current login as <alias>
    Add {
        alias: String,
        #[arg(long)]
        force: bool,
    },
    /// Switch the live login to <alias> (interactive picker if omitted)
    Use {
        alias: Option<String>,
        /// After swapping, relaunch Claude via `claude --continue`.
        #[arg(long)]
        launch: bool,
    },
    /// List saved accounts
    List {
        /// Emit machine-readable JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show the active account
    Current {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove a saved account (interactive picker if no alias given)
    Rm { alias: Option<String> },
    /// Fire a minimal request as <alias> to anchor its 5-hour window now (interactive picker if omitted)
    Prime {
        alias: Option<String>,
        /// Suppress the per-run summary line (for cron)
        #[arg(long)]
        quiet: bool,
        /// Emit machine-readable JSON (overrides --quiet)
        #[arg(long)]
        json: bool,
    },
    /// Configure scheduled window priming (no subcommand = interactive TUI)
    Schedule {
        #[command(subcommand)]
        action: Option<ScheduleAction>,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Set <alias>'s daily prime times (HH:MM, space- or comma-separated; replaces)
    Set { alias: String, times: Vec<String> },
    /// Clear one alias's schedule, or the whole schedule if omitted
    Clear { alias: Option<String> },
    /// Show the schedule
    List {
        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },
}

/// Resolve home, optional $CLAUDE_CONFIG_DIR, and the data root from the environment.
fn resolve_env() -> Result<(PathBuf, Option<PathBuf>, PathBuf)> {
    let home = PathBuf::from(std::env::var("HOME").context("HOME not set")?);
    let config_override = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    // The XDG Base Directory spec requires a relative value here to be
    // ignored, falling back to the default — not resolved against whatever
    // this process's cwd happens to be. Without this, a relative
    // XDG_DATA_HOME resolves to a different directory in every binary that
    // reads it: hooks/hook.sh's cwd, `doctor`'s cwd, install.sh's cwd are
    // all different in practice. See scripts/launcher.sh's normalize_dir_var
    // for the shell-side twin of this check (which also strips a trailing
    // slash — moot here, since PathBuf::join already suppresses a doubled
    // separator on this side; see that comment for why the two sides still
    // must agree on the rest).
    let data_root = match std::env::var_os("XDG_DATA_HOME") {
        Some(x) if !x.is_empty() && Path::new(&x).is_absolute() => {
            PathBuf::from(x).join("cc-loadout")
        }
        _ => home.join(".local").join("share").join("cc-loadout"),
    };
    Ok((home, config_override, data_root))
}

/// Interactive subcommands launch the hub on a TTY; without one, there is no
/// interactive UI to show, so print a one-line hint pointing at the
/// non-interactive form and exit cleanly.
fn headless_hint(what: &str, alternative: &str) -> Result<()> {
    println!("'{what}' is interactive and needs a terminal. {alternative}");
    Ok(())
}

pub(crate) fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn print_status(
    store: &Store,
    claude: &ClaudePaths,
    home: &std::path::Path,
    json: bool,
) -> Result<()> {
    let cfg_path = profile::config::profiles_path(home);
    let cfg = if cfg_path.exists() {
        Some(
            profile::config::load(&cfg_path)
                .with_context(|| format!("loading profiles from {}", cfg_path.display()))?,
        )
    } else {
        None
    };
    let cwd = std::env::current_dir()?;
    let now_local =
        time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    status::show(
        store,
        claude,
        cfg.as_ref(),
        &cwd,
        now_epoch() * 1000,
        now_local,
        json,
    )
}

/// Build an AppCtx and launch the hub pre-selected on the Accounts tab.
/// Used by the no-alias interactive account arms when stdout is a TTY.
fn launch_hub_accounts(
    store: Store,
    claude: ClaudePaths,
    home: PathBuf,
    data_root: PathBuf,
    cfg_path: PathBuf,
    registry_path: PathBuf,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    tui::run(
        tui::ctx::AppCtx {
            store,
            claude,
            home,
            data_root,
            cfg_path,
            registry_path,
            cwd,
        },
        tui::TAB_ACCOUNTS,
    )
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let (home, config_override, data_root) = resolve_env()?;
    let claude: ClaudePaths = paths::resolve(&home, config_override.as_deref());
    let store = Store::new(&data_root);

    match cli.command {
        None => {
            use std::io::IsTerminal;
            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                let cfg_path = profile::config::profiles_path(&home);
                let registry_path =
                    profile::discover::resolve_registry_path(&home, config_override.as_deref());
                let cwd = std::env::current_dir()?;
                let ctx = tui::ctx::AppCtx {
                    store,
                    claude,
                    home,
                    data_root,
                    cfg_path,
                    registry_path,
                    cwd,
                };
                tui::run(ctx, tui::TAB_OVERVIEW)?;
            } else {
                print_status(&store, &claude, &home, false)?;
            }
        }
        Some(command) => match command {
            Command::Profile { action } => {
                let cfg_path = profile::config::profiles_path(&home);
                let load_cfg = || {
                    profile::config::load(&cfg_path)
                        .with_context(|| format!("loading profiles from {}", cfg_path.display()))
                };
                match action {
                    ProfileAction::Init { root, assign, json } => {
                        use std::io::IsTerminal;
                        if let Some(assign_path) = assign {
                            // Non-interactive (headless / AI) mode.
                            let root = match root {
                                Some(r) => r,
                                None => anyhow::bail!("--assign requires --root <path>"),
                            };
                            let assign_json = if assign_path.as_os_str() == "-" {
                                use std::io::Read;
                                let mut s = String::new();
                                std::io::stdin()
                                    .read_to_string(&mut s)
                                    .context("reading --assign from stdin")?;
                                s
                            } else {
                                std::fs::read_to_string(&assign_path)
                                    .with_context(|| format!("reading {}", assign_path.display()))?
                            };
                            let registry_path = profile::discover::resolve_registry_path(
                                &home,
                                config_override.as_deref(),
                            );
                            let settings_path = profile::apply::global_settings_path(&claude);
                            profile::init::init_noninteractive(
                                &registry_path,
                                &root.to_string_lossy(),
                                &assign_json,
                                &cfg_path,
                                &settings_path,
                                crate::now_epoch(),
                                json,
                            )?;
                        } else {
                            if root.is_some() {
                                eprintln!(
                                    "warning: --root is not wired into the interactive wizard; \
                                     it prompts for the scan root. Ignoring --root."
                                );
                            }
                            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                                let registry_path = profile::discover::resolve_registry_path(
                                    &home,
                                    config_override.as_deref(),
                                );
                                let cwd = std::env::current_dir()?;
                                tui::run(
                                    tui::ctx::AppCtx {
                                        store,
                                        claude,
                                        home,
                                        data_root,
                                        cfg_path,
                                        registry_path,
                                        cwd,
                                    },
                                    tui::TAB_PROFILE,
                                )?;
                            } else {
                                headless_hint(
                                    "profile init",
                                    "Run it in a terminal to use the wizard, or pass --assign for non-interactive setup.",
                                )?;
                            }
                        }
                    }
                    ProfileAction::Edit => {
                        use std::io::IsTerminal;
                        if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                            let registry_path = profile::discover::resolve_registry_path(
                                &home,
                                config_override.as_deref(),
                            );
                            let cwd = std::env::current_dir()?;
                            tui::run(
                                tui::ctx::AppCtx {
                                    store,
                                    claude,
                                    home,
                                    data_root,
                                    cfg_path,
                                    registry_path,
                                    cwd,
                                },
                                tui::TAB_PROFILE,
                            )?;
                        } else {
                            headless_hint(
                                "profile edit",
                                "Run it in a terminal to use the wizard.",
                            )?;
                        }
                    }
                    ProfileAction::Detect { path, all, json } => profile::run(
                        &load_cfg()?,
                        profile::Action::Detect,
                        path,
                        all,
                        json,
                        false,
                    )?,
                    ProfileAction::Apply {
                        path,
                        all,
                        json,
                        dry_run,
                    } => profile::run(
                        &load_cfg()?,
                        profile::Action::Apply,
                        path,
                        all,
                        json,
                        dry_run,
                    )?,
                    ProfileAction::Status { path, all, json } => profile::run(
                        &load_cfg()?,
                        profile::Action::Status,
                        path,
                        all,
                        json,
                        false,
                    )?,
                    ProfileAction::Prune {
                        keys,
                        path,
                        all,
                        dry_run,
                    } => profile::prune(&load_cfg()?, &keys, path, all, dry_run)?,
                    ProfileAction::Force { names } => {
                        let cwd = std::env::current_dir()?;
                        profile::force(&cwd, &names, &load_cfg()?)?;
                    }
                    ProfileAction::Inventory { json, root } => {
                        let cfg = if cfg_path.exists() {
                            load_cfg()?
                        } else {
                            profile::config::Profiles::default()
                        };
                        profile::inventory(&cfg, json, root, &home, config_override.as_deref())?
                    }
                    ProfileAction::OnDemand { action } => {
                        let cwd = std::env::current_dir()?;
                        match action {
                            OnDemandAction::List { json } => {
                                profile::on_demand::cli_list(&cwd, &load_cfg()?, json)?
                            }
                            OnDemandAction::Acquire { key } => {
                                profile::on_demand::cli_acquire(&cwd, &load_cfg()?, &key)?
                            }
                            OnDemandAction::Release {
                                key,
                                session_id,
                                all,
                                force,
                            } => {
                                profile::on_demand::cli_release(&cwd, key, session_id, all, force)?
                            }
                        }
                    }
                }
            }
            Command::Status { json } => {
                print_status(&store, &claude, &home, json)?;
            }
            Command::Account { action } => match action {
                None => {
                    use std::io::IsTerminal;
                    if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                        let cfg_path = profile::config::profiles_path(&home);
                        let registry_path = profile::discover::resolve_registry_path(
                            &home,
                            config_override.as_deref(),
                        );
                        launch_hub_accounts(
                            store,
                            claude,
                            home,
                            data_root,
                            cfg_path,
                            registry_path,
                        )?;
                    } else {
                        headless_hint("account", "Pass an alias: cc-loadout account use <alias>.")?;
                    }
                }
                Some(action) => match action {
                    AccountAction::Add { alias, force } => {
                        account::add(&store, &claude, &alias, force, now_epoch())?;
                        println!("added account '{alias}'");
                    }
                    AccountAction::Use { alias, launch } => match alias {
                        None => {
                            use std::io::IsTerminal;
                            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                                let cfg_path = profile::config::profiles_path(&home);
                                let registry_path = profile::discover::resolve_registry_path(
                                    &home,
                                    config_override.as_deref(),
                                );
                                launch_hub_accounts(
                                    store,
                                    claude,
                                    home,
                                    data_root,
                                    cfg_path,
                                    registry_path,
                                )?;
                            } else {
                                headless_hint(
                                    "account use",
                                    "Pass an alias: cc-loadout account use <alias>.",
                                )?;
                            }
                        }
                        Some(alias) => {
                            let outcome =
                                account::swap::switch(&store, &claude, &home, &alias, now_epoch())?;
                            if let Some(w) = &outcome.warning {
                                eprintln!("warning: {w}");
                            }
                            println!("switched to '{}'", outcome.to);
                            if launch {
                                if crate::util::claude_on_path() {
                                    crate::util::exec_claude_continue()?;
                                } else {
                                    eprintln!(
                                    "warning: --launch requested but `claude` was not found on PATH; \
                                     credentials are swapped — start Claude yourself to use the new account"
                                );
                                }
                            } else {
                                println!("(credentials swapped — restart Claude or run `claude --continue` to use it)");
                            }
                        }
                    },
                    AccountAction::List { json } => {
                        let rows = account::list(&store, &claude)?;
                        let now_ms = now_epoch() * 1000;
                        if json {
                            let accounts = rows
                                .iter()
                                .map(|r| account::json::account_json(r, now_ms))
                                .collect();
                            crate::json::emit(&account::json::AccountListJson { accounts })?;
                        } else {
                            if rows.is_empty() {
                                println!("no accounts saved (run: cc-loadout account add <alias>)");
                            }
                            for r in rows {
                                let marker = if r.is_active { "*" } else { " " };
                                let org = r.meta.org_name.as_deref().unwrap_or("personal");
                                let token = account::render_token_status(
                                    r.expires_at_ms,
                                    r.has_refresh,
                                    now_ms,
                                );
                                println!(
                                    "{marker} {:<12} {} [{}] token:{token}",
                                    r.alias, r.meta.email, org
                                );
                            }
                        }
                    }
                    AccountAction::Current { json } => {
                        let active = account::current(&store)?;
                        if json {
                            crate::json::emit(&account::json::CurrentJson { active })?;
                        } else {
                            match active {
                                Some(a) => println!("{a}"),
                                None => println!("(none)"),
                            }
                        }
                    }
                    AccountAction::Rm { alias } => match alias {
                        None => {
                            use std::io::IsTerminal;
                            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                                let cfg_path = profile::config::profiles_path(&home);
                                let registry_path = profile::discover::resolve_registry_path(
                                    &home,
                                    config_override.as_deref(),
                                );
                                launch_hub_accounts(
                                    store,
                                    claude,
                                    home,
                                    data_root,
                                    cfg_path,
                                    registry_path,
                                )?;
                            } else {
                                headless_hint(
                                    "account rm",
                                    "Pass an alias: cc-loadout account rm <alias>.",
                                )?;
                            }
                        }
                        Some(alias) => {
                            account::remove(&store, &alias)?;
                            println!("removed account '{alias}'");
                        }
                    },
                    AccountAction::Prime { alias, quiet, json } => match alias {
                        None => {
                            use std::io::IsTerminal;
                            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                                let cfg_path = profile::config::profiles_path(&home);
                                let registry_path = profile::discover::resolve_registry_path(
                                    &home,
                                    config_override.as_deref(),
                                );
                                launch_hub_accounts(
                                    store,
                                    claude,
                                    home,
                                    data_root,
                                    cfg_path,
                                    registry_path,
                                )?;
                            } else {
                                headless_hint(
                                    "account prime",
                                    "Pass an alias: cc-loadout account prime <alias>.",
                                )?;
                            }
                        }
                        Some(alias) => {
                            let now = now_epoch();
                            let outcome =
                                account::prime::prime(&store, &claude, &home, &alias, "ok", now)?;
                            let (outcome_str, last_primed) = match outcome {
                                account::prime::PrimeOutcome::Primed => ("primed", Some(now)),
                                account::prime::PrimeOutcome::SkippedActive => {
                                    ("skipped_active", None)
                                }
                            };
                            if json {
                                crate::json::emit(&account::json::PrimeJson {
                                    alias: alias.clone(),
                                    outcome: outcome_str,
                                    last_primed,
                                })?;
                            } else if !quiet {
                                match outcome {
                                account::prime::PrimeOutcome::Primed => {
                                    println!("primed '{alias}' (its 5-hour window is now anchored)")
                                }
                                account::prime::PrimeOutcome::SkippedActive => println!(
                                    "'{alias}' is the active account — skipped; its window opens on your next use"
                                ),
                            }
                            }
                        }
                    },
                    AccountAction::Schedule { action } => match action {
                        None => {
                            use std::io::IsTerminal;
                            if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                                let cfg_path = profile::config::profiles_path(&home);
                                let registry_path = profile::discover::resolve_registry_path(
                                    &home,
                                    config_override.as_deref(),
                                );
                                let cwd = std::env::current_dir()?;
                                tui::run(
                                    tui::ctx::AppCtx {
                                        store,
                                        claude,
                                        home,
                                        data_root,
                                        cfg_path,
                                        registry_path,
                                        cwd,
                                    },
                                    tui::TAB_SCHEDULE,
                                )?;
                            } else {
                                headless_hint(
                                    "account schedule",
                                    "Use: cc-loadout account schedule set <alias> <HH:MM…>.",
                                )?;
                            }
                        }
                        Some(ScheduleAction::Set { alias, times }) => {
                            let joined = times.join(" ");
                            let parsed = crate::task::config::parse_times(&joined)?;
                            let def = crate::task::config::TaskDef {
                                kind: crate::task::config::Kind::Prime,
                                account: alias.clone(),
                                times: parsed.clone(),
                                prompt: None,
                                cwd: None,
                                profile: None,
                                model: None,
                                last_session_id: None,
                                last_config_dir: None,
                                last_run: None,
                                last_status: None,
                            };
                            crate::task::ops::add(
                                &crate::account::crontab::resolve_bin()?,
                                &store,
                                &data_root,
                                &home,
                                &alias,
                                def,
                            )?;
                            println!(
                                "scheduled '{alias}': {} (crontab updated)",
                                parsed.join(" ")
                            );
                        }
                        Some(ScheduleAction::Clear { alias }) => match &alias {
                            Some(a) => {
                                crate::task::ops::remove_prime(
                                    &crate::account::crontab::resolve_bin()?,
                                    &data_root,
                                    &home,
                                    a,
                                )?;
                                println!("cleared schedule for '{a}' (crontab updated)");
                            }
                            None => {
                                crate::task::ops::write_prime_schedule(
                                    &crate::account::crontab::resolve_bin()?,
                                    &data_root,
                                    &home,
                                    &std::collections::BTreeMap::new(),
                                )?;
                                println!("cleared the whole schedule (crontab block removed)");
                            }
                        },
                        Some(ScheduleAction::List { json }) => {
                            let entries = crate::task::ops::load_prime_times(&data_root)?;
                            let state = store.load_state()?;
                            let now_local = time::OffsetDateTime::now_local()
                                .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
                            let now_epoch = now_epoch();
                            if json {
                                crate::json::emit(&account::json::schedule_list_json(
                                    &entries, &state, now_local,
                                ))?;
                            } else if entries.is_empty() {
                                println!("(no schedule)");
                            } else {
                                for (alias, times) in &entries {
                                    println!("{alias}: {}", times.join("  "));
                                    if let Some(nf) = account::timing::next_fire(times, now_local) {
                                        let rel = account::format_duration_short(
                                            (nf.unix_timestamp() - now_epoch) * 1000,
                                        )
                                        .unwrap_or_else(|| "soon".into());
                                        println!(
                                            "  next: {} {:02}:{:02} (in {rel})",
                                            nf.date(),
                                            nf.hour(),
                                            nf.minute()
                                        );
                                    }
                                    match state.accounts.get(alias).and_then(|m| m.last_primed) {
                                        Some(lp) => {
                                            let ago = account::format_duration_short(
                                                (now_epoch - lp) * 1000,
                                            )
                                            .unwrap_or_else(|| "just now".into());
                                            println!("  last primed: {ago} ago");
                                        }
                                        None => println!("  last primed: never"),
                                    }
                                }
                            }
                        }
                    },
                },
            },
            Command::Task { action } => match action {
                Some(action) => {
                    let live_plugins = claude
                        .credentials
                        .parent()
                        .map(|d| d.join("plugins"))
                        .unwrap_or_else(|| home.join(".claude").join("plugins"));
                    match action {
                        TaskAction::Add {
                            id,
                            account,
                            times,
                            prompt,
                            cwd,
                            profile,
                            model,
                        } => {
                            let parsed = crate::task::config::parse_times(&times.join(","))?;
                            let kind = if prompt.is_some() {
                                task::config::Kind::Task
                            } else {
                                task::config::Kind::Prime
                            };
                            let def = task::config::TaskDef {
                                kind,
                                account,
                                times: parsed,
                                prompt,
                                cwd,
                                profile,
                                model,
                                last_session_id: None,
                                last_config_dir: None,
                                last_run: None,
                                last_status: None,
                            };
                            task::ops::add(
                                &crate::account::crontab::resolve_bin()?,
                                &store,
                                &data_root,
                                &home,
                                &id,
                                def,
                            )?;
                            println!("added task '{id}'");
                        }
                        TaskAction::Rm { id } => {
                            task::ops::remove(
                                &crate::account::crontab::resolve_bin()?,
                                &data_root,
                                &home,
                                &id,
                            )?;
                            println!("removed task '{id}'");
                        }
                        TaskAction::List { json } => task::ops::list(&data_root, json)?,
                        TaskAction::Run { id, quiet } => {
                            let out = task::run::run_task(
                                &store,
                                &data_root,
                                &home,
                                &live_plugins,
                                &id,
                                now_epoch(),
                            )?;
                            if !quiet {
                                println!("{out:?}");
                            }
                        }
                        TaskAction::Resume { id } => {
                            task::resume::resume(&store, &data_root, &home, &id)?
                        }
                    }
                }
                None => {
                    use std::io::IsTerminal;
                    if tui::should_launch_tui(std::io::stdout().is_terminal()) {
                        let cfg_path = profile::config::profiles_path(&home);
                        let registry_path = profile::discover::resolve_registry_path(
                            &home,
                            config_override.as_deref(),
                        );
                        let cwd = std::env::current_dir()?;
                        tui::run(
                            tui::ctx::AppCtx {
                                store,
                                claude,
                                home,
                                data_root,
                                cfg_path,
                                registry_path,
                                cwd,
                            },
                            tui::TAB_TASKS,
                        )?;
                    } else {
                        headless_hint(
                            "task",
                            "Use: cc-loadout task add <id> --account <a> --at <HH:MM…> --prompt <p> --cwd <dir>.",
                        )?;
                    }
                }
            },
            Command::Hook { action } => {
                use std::io::Read;
                let mut raw = String::new();
                let _ = std::io::stdin().read_to_string(&mut raw);
                match action {
                    HookAction::SessionStart => {
                        hooks::session_start(&home, config_override.as_deref(), &raw)?
                    }
                    HookAction::SessionEnd => hooks::session_end(&raw)?,
                }
            }
            Command::Doctor { fix, prune_backups } => {
                // Resolved here, not inside doctor, so the convergence step
                // stays a pure function of its inputs. `current_exe()` follows
                // symlinks, so invoking the launcher's ~/.local/bin symlink
                // still reports the data-dir path its guard looks for.
                let exe = std::env::current_exe().ok();
                let report = doctor::run(
                    &home,
                    config_override.as_deref(),
                    &data_root,
                    exe.as_deref(),
                    fix,
                    prune_backups,
                )?;
                doctor::print(&report, fix);
            }
        }, // Some(command) => match command
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
