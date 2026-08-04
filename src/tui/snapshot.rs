use std::path::PathBuf;

use anyhow::Result;
use time::OffsetDateTime;

use crate::account::{self};
use crate::profile::{apply, config, detect};
use crate::tui::ctx::AppCtx;
use crate::tui::widgets::latest;

/// One account row, with the raw fields the Overview needs to recompute live.
pub struct AcctRow {
    pub alias: String,
    pub email: String,
    /// Organization name, or `None` when the account has no org (so views can
    /// omit it rather than showing a redundant default like "personal").
    pub org: Option<String>,
    pub is_active: bool,
    pub expires_at_ms: Option<i64>,
    pub has_refresh: bool,
    /// max(last_used, last_primed) epoch-seconds — a *proxy* anchor for the 5h
    /// usage window. The true window is server-side, anchored to the account's
    /// first real request; `last_used` is swap-commit time and `last_primed` is
    /// `None` for the active account (prime skips it), so the active-account
    /// gauge derived from this is approximate, not authoritative.
    pub window_start_epoch: Option<i64>,
}

/// One task row for the Tasks tab (covers both `prime` and `task` kinds).
#[derive(Clone)]
pub struct TaskRow {
    pub id: String,
    pub kind: crate::task::config::Kind,
    pub account: String,
    pub times: Vec<String>,
    pub next_fire: Option<OffsetDateTime>,
    /// The model this run will actually pass to `claude` — `None` ⇒ none pinned,
    /// so the CLI picks. Resolved via `TaskDef::effective_model`, which is why a
    /// prime shows `haiku` even when its entry pins nothing.
    pub model: Option<String>,
    pub last_status: Option<String>,
}

/// One scheduled-account row.
pub struct PrimeRow {
    pub alias: String,
    pub next_fire: Option<OffsetDateTime>,
    /// Rendered by the Plan 03 Schedule tab; gathered now with `next_fire`.
    pub last_primed: Option<i64>,
}

/// All data the hub renders, gathered once. Time math is recomputed per-frame
/// from these fields, so a tick never re-reads disk.
pub struct Snapshot {
    pub accounts: Vec<AcctRow>,
    pub cwd: PathBuf,
    pub profiles_json_exists: bool,
    pub matched: Vec<String>,
    pub applied_count: usize,
    pub priming: Vec<PrimeRow>,
    /// Raw alias -> times map cloned from `schedule.json` before priming-loop
    /// consumes the entries. Used by `ScheduleView` and the two accessors below.
    pub schedule: std::collections::BTreeMap<String, Vec<String>>,
    /// Plugin keys currently enabled in the global ~/.claude/settings.json.
    /// Re-read each load so the Profile tab's global-drift badge clears after a
    /// commit re-syncs the global set.
    pub global_enabled: Vec<String>,
    /// All scheduled tasks (prime + real) from `tasks.json`.
    pub tasks: Vec<TaskRow>,
    /// True when the live crontab's managed block diverges from what `tasks.json`
    /// would install — i.e. the saved schedule is NOT what cron actually holds
    /// (a write that never landed, or a crontab wiped externally). Computed once
    /// per load (best-effort; any crontab-read error is treated as no drift).
    pub schedule_drift: bool,
    /// Managed plugin keys whose registry scope drifted off `user`. Best-effort:
    /// a malformed profiles.json or missing registry yields an empty vec so the
    /// hub still opens, matching how `schedule_drift` degrades.
    pub scope_drift: Vec<String>,
}

impl Snapshot {
    /// The scheduled times for `alias`, if it is in the schedule.
    pub fn schedule_times(&self, alias: &str) -> Option<&Vec<String>> {
        self.schedule.get(alias)
    }

    /// Last-primed epoch-seconds for `alias`, if known.
    pub fn last_primed(&self, alias: &str) -> Option<i64> {
        self.priming
            .iter()
            .find(|p| p.alias == alias)
            .and_then(|p| p.last_primed)
    }

    pub fn load(ctx: &AppCtx) -> Result<Snapshot> {
        let rows = account::list(&ctx.store, &ctx.claude)?;

        let accounts = rows
            .into_iter()
            .map(|r| AcctRow {
                org: r.meta.org_name.clone(),
                email: r.meta.email.clone(),
                window_start_epoch: latest(r.meta.last_used, r.meta.last_primed),
                alias: r.alias,
                is_active: r.is_active,
                expires_at_ms: r.expires_at_ms,
                has_refresh: r.has_refresh,
            })
            .collect();

        let schedule = crate::task::ops::load_prime_times(&ctx.data_root)?;
        let state = ctx.store.load_state()?;
        let now_local = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let priming = schedule
            .iter()
            .map(|(alias, times)| PrimeRow {
                next_fire: account::timing::next_fire(times, now_local),
                last_primed: state.accounts.get(alias).and_then(|m| m.last_primed),
                alias: alias.clone(),
            })
            .collect();

        let (profiles_json_exists, matched, applied_count) = if ctx.cfg_path.exists() {
            // Degrade gracefully on a malformed profiles.json so that all tabs
            // still open (the board can then repair the config).
            if let Ok(cfg) = config::load(&ctx.cfg_path) {
                let matched = detect::detect_profiles(&ctx.cwd, &cfg);
                let applied = apply::enabled_keys(&ctx.cwd)?;
                (true, matched, applied.len())
            } else {
                (true, Vec::new(), 0)
            }
        } else {
            (false, Vec::new(), 0)
        };

        let global_enabled = apply::read_global_enabled(&apply::global_settings_path(&ctx.claude))
            .unwrap_or_default();

        let all_tasks =
            crate::task::config::load(&crate::task::config::tasks_path(&ctx.data_root))?;
        let tasks = all_tasks
            .tasks
            .into_iter()
            .map(|(id, d)| TaskRow {
                next_fire: account::timing::next_fire(&d.times, now_local),
                model: d.effective_model().map(str::to_string),
                id,
                kind: d.kind,
                account: d.account,
                times: d.times,
                last_status: d.last_status,
            })
            .collect();

        // Best-effort: is the live crontab out of sync with the saved schedule?
        // Any resolve/read failure counts as "no drift" so the hub still opens; the
        // loud signal for a missing/unwritable crontab is at write time.
        let schedule_drift = crate::account::crontab::resolve_bin()
            .and_then(|bin| crate::task::ops::schedule_drift(&bin, &ctx.data_root, &ctx.home))
            .unwrap_or(false);

        let scope_drift = config::load(&ctx.cfg_path)
            .map(|cfg| crate::profile::registry::keys_needing_promotion(&cfg, &ctx.registry_path))
            .unwrap_or_default();

        Ok(Snapshot {
            accounts,
            cwd: ctx.cwd.clone(),
            profiles_json_exists,
            matched,
            applied_count,
            priming,
            schedule,
            global_enabled,
            tasks,
            schedule_drift,
            scope_drift,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::paths;
    use crate::account::store::Store;

    fn ctx_with_login(home: &std::path::Path, data: &std::path::Path) -> AppCtx {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","expiresAt":9999999999999,"refreshToken":"r"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"a@b.com"}}"#,
        )
        .unwrap();
        AppCtx {
            store: Store::new(data),
            claude: paths::resolve(home, None),
            home: home.to_path_buf(),
            data_root: data.to_path_buf(),
            cfg_path: home.join("nope-profiles.json"),
            registry_path: home.join("nope-registry.json"),
            cwd: home.to_path_buf(),
        }
    }

    #[test]
    fn load_reports_active_account_and_no_profiles() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_login(hdir.path(), ddir.path());
        account::add(&ctx.store, &ctx.claude, "work", false, 1).unwrap();

        let snap = Snapshot::load(&ctx).unwrap();
        assert!(snap
            .accounts
            .iter()
            .any(|a| a.is_active && a.alias == "work"));
        assert_eq!(snap.accounts.len(), 1);
        assert!(snap.accounts[0].is_active);
        assert_eq!(snap.accounts[0].expires_at_ms, Some(9999999999999));
        assert!(!snap.profiles_json_exists);
        assert!(snap.priming.is_empty());
    }

    #[test]
    fn load_reads_global_enabled_plugins() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_login(hdir.path(), ddir.path());
        // write a global settings.json with one enabled + one disabled plugin
        std::fs::write(
            hdir.path().join(".claude").join("settings.json"),
            r#"{"enabledPlugins":{"on@m":true,"off@m":false}}"#,
        )
        .unwrap();
        let snap = Snapshot::load(&ctx).unwrap();
        assert_eq!(snap.global_enabled, vec!["on@m".to_string()]);
    }
}
