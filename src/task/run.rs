use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::account::store::Store;
use crate::account::{creds, paths};
use crate::profile::{apply as papply, config as pconfig};
use crate::task::config;
use crate::task::{exec, sandbox};

/// How long a real task may run before it is killed. Far longer than prime's
/// 120s ping budget — real tasks do work.
const TASK_TIMEOUT_SECS: u64 = 1800;

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The task's `claude -p` ran — against the live ~/.claude for the active
    /// account, or an isolated config dir for a non-active account.
    Ran,
}

pub fn run_task(
    store: &Store,
    data_root: &Path,
    home: &Path,
    live_plugins: &Path,
    id: &str,
    now: i64,
) -> Result<RunOutcome> {
    run_task_with(
        Path::new("claude"),
        store,
        data_root,
        home,
        live_plugins,
        id,
        now,
    )
}

pub(crate) fn run_task_with(
    claude_bin: &Path,
    store: &Store,
    data_root: &Path,
    home: &Path,
    live_plugins: &Path,
    id: &str,
    now: i64,
) -> Result<RunOutcome> {
    let _lock = crate::util::lock::acquire(&store.lock_path())?;

    let tasks_path = config::tasks_path(data_root);
    let mut tasks = config::load(&tasks_path)?;
    let def = tasks
        .tasks
        .get(id)
        .ok_or_else(|| anyhow::anyhow!("unknown task '{id}'"))?
        .clone();
    def.validate()?;

    let state = store.load_state()?;
    if !state.accounts.contains_key(&def.account) {
        bail!("task '{id}' names unknown account '{}'", def.account);
    }
    let active = state.active_alias.as_deref();

    // A prime on the *active* account is deliberately NOT skipped. Being the
    // active account — or merely having an idle Claude session open — does not
    // mean the 5-hour usage window is open: only a real request opens it, and
    // it lapses 5h later (e.g. overnight). run_location() routes the active
    // account to RunLocation::Live, so the prime fires against the live
    // ~/.claude, sharing Claude Code's cross-process refresh lock (token-safe)
    // and anchoring the window — which is the whole point of a pre-work prime.
    let location = run_location(&def.account, active);

    // Choose config dir + cwd.
    let (cfg_dir, cwd): (Option<PathBuf>, PathBuf) = match location {
        RunLocation::Live => (None, def.cwd.clone().unwrap_or_else(|| home.to_path_buf())),
        RunLocation::Isolated => {
            let dir = sandbox::ensure_isolated_dir(store, &def.account, live_plugins, home)?;
            (
                Some(dir),
                def.cwd.clone().unwrap_or_else(|| home.to_path_buf()),
            )
        }
    };

    // Apply the profile to the cwd, if any.
    if let Some(profile) = &def.profile {
        let cfg_file = pconfig::profiles_path(home);
        let cfg = pconfig::load(&cfg_file)?;
        papply::apply(&cwd, &cfg, std::slice::from_ref(profile))?;
    }

    let prompt = def.prompt.clone().unwrap_or_else(|| "ok".to_string());
    let session_id = uuid::Uuid::new_v4().to_string();

    let result = exec::run_claude(
        claude_bin,
        &exec::RunSpec {
            cfg_dir: cfg_dir.as_deref(),
            cwd: &cwd,
            prompt: &prompt,
            session_id: &session_id,
            // `None` here means "no --model flag" — the CLI then resolves its own
            // default. Primes resolve to the cheap ping model instead.
            model: def.effective_model(),
            timeout: Duration::from_secs(TASK_TIMEOUT_SECS),
            // Probe pings have no distill-worthy transcript — suppress the cortex
            // Raw. Real tasks stay recorded.
            skip_record: def.kind == config::Kind::Prime,
        },
    );

    // Sync rotated credentials back to the snapshot (isolated path only).
    if let Some(dir) = &cfg_dir {
        let eph = paths::resolve(home, Some(dir));
        if let Ok(rotated) = creds::read_credentials(&eph.credentials) {
            let snap = store.credentials_snapshot(&def.account);
            if creds::read_credentials(&snap).ok() != Some(rotated.clone()) {
                let _ = creds::write_credentials(&snap, &rotated);
            }
        }
    }

    // Record outcome.
    if let Some(d) = tasks.tasks.get_mut(id) {
        d.last_run = Some(now);
        match &result {
            Ok(()) => {
                d.last_session_id = Some(session_id);
                d.last_config_dir = cfg_dir.clone();
                d.last_status = Some("ok".into());
            }
            Err(e) => {
                d.last_status = Some(format!("error: {e}"));
            }
        }
    }
    config::save(&tasks_path, &tasks)?;

    // A successful run anchors the account's window — record it so status / schedule
    // list reflect cron-driven primes (not just manual `account prime`).
    if result.is_ok() {
        let mut st = store.load_state()?;
        if let Some(meta) = st.accounts.get_mut(&def.account) {
            meta.last_primed = Some(now);
        }
        store.save_state(&st)?;
    }

    result.map(|()| RunOutcome::Ran)
}

/// Where a scheduled run executes. The choice exists purely to keep OAuth token
/// rotation safe: running the *active* account in the live ~/.claude shares
/// Claude Code's cross-process refresh lock, while a non-active account must run
/// in an isolated copy to avoid colliding with the live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunLocation {
    /// The real ~/.claude (no CLAUDE_CONFIG_DIR override).
    Live,
    /// A persistent, isolated per-account config dir.
    Isolated,
}

/// Live iff the named account is the current foreground account.
pub fn run_location(account: &str, active: Option<&str>) -> RunLocation {
    if active == Some(account) {
        RunLocation::Live
    } else {
        RunLocation::Isolated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_account_runs_live() {
        assert_eq!(run_location("work", Some("work")), RunLocation::Live);
    }

    #[test]
    fn non_active_account_runs_isolated() {
        assert_eq!(
            run_location("work", Some("personal")),
            RunLocation::Isolated
        );
        assert_eq!(run_location("work", None), RunLocation::Isolated);
    }

    /// Run one task through `run_task_with` against a fake claude that dumps its
    /// argv into `<cwd>/argv-dump`, and return those words. The account is never
    /// the active one, so the run takes the isolated path.
    fn argv_of_run(kind: crate::task::config::Kind, model: Option<&str>) -> Vec<String> {
        use crate::account::creds;
        use crate::account::store::{AccountMeta, Store};
        use crate::task::config::{self, TaskDef};
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        let claude = bin.path().join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\n: > argv-dump\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> argv-dump; done\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::new(data.path());
        store.ensure_account_dir("work").unwrap();
        creds::write_credentials(
            &store.credentials_snapshot("work"),
            &json!({"claudeAiOauth":{"accessToken":"A","refreshToken":"R"}}),
        )
        .unwrap();
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        st.active_alias = Some("other".into()); // not active → isolated
        store.save_state(&st).unwrap();

        let tp = config::tasks_path(data.path());
        let mut tasks = config::Tasks::default();
        tasks.tasks.insert(
            "t".into(),
            TaskDef {
                kind,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(cwd.path().to_path_buf()),
                profile: None,
                model: model.map(str::to_string),
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&tp, &tasks).unwrap();

        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();
        run_task_with(
            &claude,
            &store,
            data.path(),
            home.path(),
            &live_plugins,
            "t",
            1700,
        )
        .unwrap();

        std::fs::read_to_string(cwd.path().join("argv-dump"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        let i = argv.iter().position(|w| w == flag)?;
        argv.get(i + 1).map(String::as_str)
    }

    #[test]
    fn a_scheduled_prime_is_forced_onto_the_cheap_ping_model() {
        let argv = argv_of_run(crate::task::config::Kind::Prime, None);
        assert_eq!(
            flag_value(&argv, "--model"),
            Some(crate::task::config::PING_MODEL),
            "a prime must not burn the account default: {argv:?}"
        );
    }

    #[test]
    fn a_task_without_a_model_passes_no_model_flag() {
        let argv = argv_of_run(crate::task::config::Kind::Task, None);
        assert!(
            !argv.iter().any(|w| w == "--model"),
            "an unpinned task must inherit the CLI default: {argv:?}"
        );
    }

    #[test]
    fn a_pinned_task_model_reaches_claude() {
        let argv = argv_of_run(crate::task::config::Kind::Task, Some("claude-sonnet-4-6"));
        assert_eq!(flag_value(&argv, "--model"), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn a_pinned_prime_model_overrides_the_ping_default() {
        let argv = argv_of_run(crate::task::config::Kind::Prime, Some("sonnet"));
        assert_eq!(flag_value(&argv, "--model"), Some("sonnet"));
    }

    #[test]
    fn task_runs_isolated_for_non_active_and_records_session() {
        use crate::account::creds;
        use crate::account::store::{AccountMeta, Store};
        use crate::task::config::{self, Kind, TaskDef};
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();

        // fake claude that just writes a marker (asserts --session-id present)
        let claude = bin.path().join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --session-id ] && { shift; : > \"$CLAUDE_CONFIG_DIR/ran-$1\"; }; shift; done\nexit 0\n",
        ).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::new(data.path());
        store.ensure_account_dir("work").unwrap();
        creds::write_credentials(
            &store.credentials_snapshot("work"),
            &json!({"claudeAiOauth":{"accessToken":"A","refreshToken":"R"}}),
        )
        .unwrap();
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        st.active_alias = Some("other".into()); // work is NOT active → isolated
        store.save_state(&st).unwrap();

        // a tasks.json with one task
        let tp = config::tasks_path(data.path());
        let mut tasks = config::Tasks::default();
        tasks.tasks.insert(
            "weekly".into(),
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(cwd.path().to_path_buf()),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&tp, &tasks).unwrap();

        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();

        let out = run_task_with(
            &claude,
            &store,
            data.path(),
            home.path(),
            &live_plugins,
            "weekly",
            1700,
        )
        .unwrap();
        assert_eq!(out, RunOutcome::Ran);

        let after = config::load(&tp).unwrap();
        let def = &after.tasks["weekly"];
        assert!(def.last_session_id.is_some(), "session id recorded");
        assert!(def.last_config_dir.is_some(), "isolated dir recorded");
        assert_eq!(def.last_status.as_deref(), Some("ok"));

        // Fix I2: last_primed must be recorded on a successful run.
        let st_after = store.load_state().unwrap();
        assert_eq!(
            st_after.accounts["work"].last_primed,
            Some(1700),
            "last_primed must be set to `now` after a successful run"
        );
    }

    #[test]
    fn prime_on_active_account_runs_live_and_records_prime() {
        // The 5-hour usage window opens on the first real request — NOT on
        // being the active account, and NOT on having an idle session open.
        // So a scheduled prime whose account is the *active* one must still
        // fire a live `claude -p` (against real ~/.claude) to open the window;
        // skipping it leaves the window closed until the user manually types.
        use crate::account::store::{AccountMeta, Store};
        use crate::task::config::{self, Kind, TaskDef};
        use std::os::unix::fs::PermissionsExt;

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();

        // A fake claude that just succeeds. The Live path sets no
        // CLAUDE_CONFIG_DIR, so the isolated test's marker-file trick has
        // nowhere to write — a bare `exit 0` is enough to prove a request was
        // fired and its outcome recorded.
        let claude = bin.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        st.active_alias = Some("work".into()); // work IS active → Live, must NOT skip
        store.save_state(&st).unwrap();

        let tp = config::tasks_path(data.path());
        let mut tasks = config::Tasks::default();
        tasks.tasks.insert(
            "wp".into(),
            TaskDef {
                kind: Kind::Prime,
                account: "work".into(),
                times: vec!["06:00".into()],
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&tp, &tasks).unwrap();
        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();

        let out = run_task_with(
            &claude,
            &store,
            data.path(),
            home.path(),
            &live_plugins,
            "wp",
            1700,
        )
        .unwrap();

        // Ran live, not skipped.
        assert_eq!(out, RunOutcome::Ran);

        let after = config::load(&tp).unwrap();
        let def = &after.tasks["wp"];
        assert_eq!(def.last_status.as_deref(), Some("ok"));
        assert!(
            def.last_session_id.is_some(),
            "a real request was fired, so a session id must be recorded"
        );
        assert!(
            def.last_config_dir.is_none(),
            "the active account primes against live ~/.claude (no isolated config dir)"
        );

        // The window is now anchored for the active account.
        let st_after = store.load_state().unwrap();
        assert_eq!(
            st_after.accounts["work"].last_primed,
            Some(1700),
            "priming the active account must record last_primed"
        );
    }

    /// Fake claude that dumps `${CORTEX_SKIP_RECORD:-}` into `env-dump` in cwd.
    fn fake_env_dump_claude(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\nprintf '%s' \"${CORTEX_SKIP_RECORD:-}\" > env-dump\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn scheduled_prime_exports_cortex_skip_record() {
        // A cron-driven probe ping must not litter the cortex vault with a
        // junk Raw — the Prime kind maps to CORTEX_SKIP_RECORD=1.
        use crate::account::store::{AccountMeta, Store};
        use crate::task::config::{self, Kind, TaskDef};

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let claude = fake_env_dump_claude(bin.path());

        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        st.active_alias = Some("work".into()); // active → Live, cwd defaults to home
        store.save_state(&st).unwrap();

        let tp = config::tasks_path(data.path());
        let mut tasks = config::Tasks::default();
        tasks.tasks.insert(
            "wp".into(),
            TaskDef {
                kind: Kind::Prime,
                account: "work".into(),
                times: vec!["06:00".into()],
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&tp, &tasks).unwrap();
        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();

        run_task_with(
            &claude,
            &store,
            data.path(),
            home.path(),
            &live_plugins,
            "wp",
            1700,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(home.path().join("env-dump")).unwrap(),
            "1",
            "a scheduled prime must export CORTEX_SKIP_RECORD=1"
        );
    }

    #[test]
    fn scheduled_task_leaves_cortex_skip_record_unset() {
        // Real scheduled tasks are work sessions — their transcripts ARE
        // distill-worthy, so cortex recording must stay on.
        use crate::account::creds;
        use crate::account::store::{AccountMeta, Store};
        use crate::task::config::{self, Kind, TaskDef};
        use serde_json::json;

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_env_dump_claude(bin.path());

        let store = Store::new(data.path());
        store.ensure_account_dir("work").unwrap();
        creds::write_credentials(
            &store.credentials_snapshot("work"),
            &json!({"claudeAiOauth":{"accessToken":"A","refreshToken":"R"}}),
        )
        .unwrap();
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        st.active_alias = Some("other".into()); // work is NOT active → isolated
        store.save_state(&st).unwrap();

        let tp = config::tasks_path(data.path());
        let mut tasks = config::Tasks::default();
        tasks.tasks.insert(
            "weekly".into(),
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(cwd.path().to_path_buf()),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&tp, &tasks).unwrap();
        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();

        run_task_with(
            &claude,
            &store,
            data.path(),
            home.path(),
            &live_plugins,
            "weekly",
            1700,
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(cwd.path().join("env-dump")).unwrap(),
            "",
            "a scheduled task must NOT export CORTEX_SKIP_RECORD"
        );
    }
}
