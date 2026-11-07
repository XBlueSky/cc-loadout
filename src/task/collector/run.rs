//! The collector run path behind `cc-loadout task run <id>` for a
//! `kind: collector` task (and for an unknown id when a collector is
//! configured). Unlike a plain task it never blocks on the cc-loadout lock,
//! never touches `tasks.json` or `state.json` for bookkeeping, and reports
//! every attempt as exactly one export record for Geralt's reporter. Only a
//! run that cannot even tell where to write that record ends with a log line
//! alone.

use std::path::{Path, PathBuf};
use std::time::Duration;

use time::OffsetDateTime;

use crate::account::store::Store;
use crate::account::{creds, paths};
use crate::task::collector::export::{
    format_instant, new_attempt_id, write_export, ExportRecord, SafeErrorCode, Status,
};
use crate::task::collector::launch::{launcher_command, Captured, OutputPumps, CLOSE_WAIT_MS};
use crate::task::collector::log::{LogLine, PrelaunchReason};
use crate::task::collector::outcome::{classify, parse_outcome_line, Observation, Observed};
use crate::task::config::{is_collector_id, CollectorSettings, CollectorTask, Kind, Tasks};
use crate::task::run::{run_location, RunLocation};
use crate::task::sandbox;
use crate::util::lock;

/// Where a collector run finds the things it shares with every other cc-loadout
/// command.
pub struct RunCtx<'a> {
    pub store: &'a Store,
    pub home: &'a Path,
    pub live_plugins: &'a Path,
}

/// Whether `task run <id>` belongs to the collector path: the task exists and is
/// a collector, or it does not exist and a collector is configured (so the
/// attempt can still be recorded as `launch_failed`). Without a collector
/// section an unknown id keeps the plain "unknown task" error.
pub fn routes_to_collector(tasks: &Tasks, id: &str) -> bool {
    match tasks.tasks.get(id) {
        Some(def) => def.kind == Kind::Collector,
        None => tasks.collector.is_some(),
    }
}

/// Run one collector attempt for task `id` and return the process exit code:
/// 0 only for a clean `exited_ok`, 1 for everything else. Every failure has
/// already been written as an export record or a runner-log line by the time
/// this returns.
pub fn run_collector(ctx: &RunCtx<'_>, tasks: &Tasks, id: &str) -> i32 {
    run_with_log(ctx, tasks, id, &mut |line| line.emit())
}

/// What `launch_and_watch` needs from the steps before it.
struct Launch<'a> {
    task_id: &'a str,
    settings: &'a CollectorSettings,
    task: &'a CollectorTask,
    account: &'a str,
    attempt: &'a str,
    session: &'a str,
    /// `CLAUDE_CONFIG_DIR` of an isolated account; `None` runs in the live
    /// `~/.claude`.
    cfg_dir: Option<PathBuf>,
}

/// [`run_collector`] with the runner-log sink made explicit, so tests can read
/// the lines instead of the process's stderr.
fn run_with_log(
    ctx: &RunCtx<'_>,
    tasks: &Tasks,
    id: &str,
    log: &mut dyn FnMut(&LogLine<'_>),
) -> i32 {
    // 1. Without valid machine-wide settings there is no export directory and
    //    no runner identity to write a record with.
    let Some(settings) = tasks.collector.as_ref() else {
        log(&LogLine::Unconfigured);
        return 1;
    };
    if settings.validate().is_err() {
        log(&LogLine::SettingsInvalid);
        return 1;
    }

    // 2. Both IDs exist before the lock is taken, so even a busy attempt has
    //    its own record name.
    let attempt = new_attempt_id();
    let session = uuid::Uuid::new_v4().to_string();

    // 3. An id outside the grammar cannot be a record's `taskId`.
    if !is_collector_id(id) {
        log(&LogLine::Prelaunch {
            attempt: &attempt,
            reason: PrelaunchReason::InvalidTaskId,
        });
        return 1;
    }
    let launch_failed = |reason: PrelaunchReason, log: &mut dyn FnMut(&LogLine<'_>)| {
        record_not_launched(
            settings,
            id,
            &attempt,
            (Status::LaunchFailed, None),
            LogLine::Prelaunch {
                attempt: &attempt,
                reason,
            },
            log,
        )
    };

    // 4. The kind is read without the lock and not re-read after it: `task
    //    add` and `task rm` do not take the lock either.
    let Some(def) = tasks.tasks.get(id) else {
        return launch_failed(PrelaunchReason::UnknownTask, log);
    };

    // 5. Never wait for another run: a busy attempt is recorded and ends.
    let _lock = match lock::try_acquire(&ctx.store.lock_path()) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return record_not_launched(
                settings,
                id,
                &attempt,
                (Status::SkippedBusy, Some(SafeErrorCode::LockBusy)),
                LogLine::Busy { attempt: &attempt },
                log,
            );
        }
        Err(_) => return launch_failed(PrelaunchReason::LockIo, log),
    };

    // 6. The process-tree supervision of a collector run is Linux-only.
    if cfg!(not(target_os = "linux")) {
        return launch_failed(PrelaunchReason::UnsupportedPlatform, log);
    }
    let task = match (&def.collector, def.kind == Kind::Collector, def.validate()) {
        (Some(task), true, Ok(())) => task,
        _ => return launch_failed(PrelaunchReason::InvalidTask, log),
    };

    // 7. The account the run executes as, and where its Claude config lives.
    let Ok(state) = ctx.store.load_state() else {
        return launch_failed(PrelaunchReason::StateUnreadable, log);
    };
    if !state.accounts.contains_key(&def.account) {
        return launch_failed(PrelaunchReason::UnknownAccount, log);
    }
    let cfg_dir = match run_location(&def.account, state.active_alias.as_deref()) {
        RunLocation::Live => None,
        RunLocation::Isolated => {
            match sandbox::ensure_isolated_dir(ctx.store, &def.account, ctx.live_plugins, ctx.home)
            {
                Ok(dir) => Some(dir),
                Err(_) => return launch_failed(PrelaunchReason::IsolatedDir, log),
            }
        }
    };

    // 8. Start the launcher and watch it.
    let record = launch_and_watch(
        ctx,
        &Launch {
            task_id: id,
            settings,
            task,
            account: &def.account,
            attempt: &attempt,
            session: &session,
            cfg_dir,
        },
        log,
    );

    // 9. Write the record; the lock is released when this function returns.
    finish(settings, &record, log)
}

/// Step 8: start the launcher and supervise it until the attempt is over, then
/// describe how it ended.
fn launch_and_watch(
    ctx: &RunCtx<'_>,
    launch: &Launch<'_>,
    log: &mut dyn FnMut(&LogLine<'_>),
) -> ExportRecord {
    let watched = watch_launcher(launch, log);

    // Rotated credentials are token state, not attempt state: keep them however
    // the attempt went.
    if let Some(dir) = &launch.cfg_dir {
        sync_rotated_credentials(ctx, launch.account, dir);
    }

    // Stdout counts as an outcome line only if it was kept whole.
    let outcome = if watched.captured.stdout_truncated {
        None
    } else {
        parse_outcome_line(&watched.captured.stdout)
    };
    let c = classify(&watched.observation, &watched.seen, outcome);

    ExportRecord {
        session_id: Some(launch.session.to_string()),
        exit_code: c.exit_code,
        started_at: Some(format_instant(watched.started)),
        ended_at: Some(format_instant(watched.ended)),
        requested_model: c.requested_model,
        effective_model: c.effective_model,
        effective_policy_digest: c.effective_policy_digest,
        ..ExportRecord::not_launched(
            &launch.settings.runner_instance_id,
            launch.task_id,
            launch.attempt,
            c.status,
            c.safe_error_code,
        )
    }
}

/// What watching the launcher left behind: how it ended, what the runner itself
/// did to it, what it printed, and when.
struct Watched {
    observation: Observation,
    seen: Observed,
    captured: Captured,
    started: OffsetDateTime,
    ended: OffsetDateTime,
}

/// Start the launcher and wait for it to end.
///
/// This is the whole of the supervision for now: it waits for the launcher and
/// does nothing about its descendants or its running time, so `seen` is always
/// empty. The process-tree supervision of Task 5 replaces the wait and fills `seen`.
fn watch_launcher(launch: &Launch<'_>, log: &mut dyn FnMut(&LogLine<'_>)) -> Watched {
    let started = OffsetDateTime::now_utc();
    let spawned = crate::util::retry_etxtbsy(|| {
        launcher_command(
            launch.settings,
            launch.task,
            launch.attempt,
            launch.session,
            launch.cfg_dir.as_deref(),
        )
        .spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            return Watched {
                observation: Observation::SpawnError {
                    enoent: e.kind() == std::io::ErrorKind::NotFound,
                },
                seen: Observed::default(),
                captured: Captured {
                    stdout: Vec::new(),
                    stdout_truncated: false,
                    stderr_dropped: 0,
                },
                started,
                ended: started,
            };
        }
    };
    let pumps = OutputPumps::start(&mut child, launch.attempt);
    log(&LogLine::Started {
        attempt: launch.attempt,
    });

    let observation = match child.wait() {
        Ok(status) => status
            .code()
            .map_or(Observation::Signaled, Observation::Exited),
        Err(_) => Observation::NotReaped,
    };
    let captured = pumps.finish(Duration::from_millis(CLOSE_WAIT_MS));
    if captured.stderr_dropped > 0 {
        log(&LogLine::StderrDropped {
            attempt: launch.attempt,
            lines: captured.stderr_dropped,
        });
    }
    Watched {
        observation,
        seen: Observed::default(),
        captured,
        // Never earlier than the start, even if the wall clock stepped back.
        ended: OffsetDateTime::now_utc().max(started),
        started,
    }
}

/// Copy credentials the launcher's Claude rotated in an isolated config dir back
/// to the account's snapshot, as a plain scheduled task does after its run; a
/// refresh token that is used once is gone, so the new one must not be lost.
/// Best effort: a record of the attempt matters more than this write.
fn sync_rotated_credentials(ctx: &RunCtx<'_>, account: &str, cfg_dir: &Path) {
    let eph = paths::resolve(ctx.home, Some(cfg_dir));
    if let Ok(rotated) = creds::read_credentials(&eph.credentials) {
        let snap = ctx.store.credentials_snapshot(account);
        if creds::read_credentials(&snap).ok() != Some(rotated.clone()) {
            let _ = creds::write_credentials(&snap, &rotated);
        }
    }
}

/// Step 9: write the attempt's export record and map it to the exit code. Any
/// failure to write is treated as "the file may exist" — a record is never
/// written twice — so it is logged and the run exits 1.
fn finish(
    settings: &CollectorSettings,
    record: &ExportRecord,
    log: &mut dyn FnMut(&LogLine<'_>),
) -> i32 {
    match write_export(&settings.export_dir, record) {
        Ok(_) => {
            log(&LogLine::Recorded {
                attempt: &record.attempt_id,
                status: record.status,
                code: record.safe_error_code,
            });
            exit_code_for(record)
        }
        Err(_) => {
            log(&LogLine::ExportFailed {
                attempt: &record.attempt_id,
            });
            1
        }
    }
}

/// 0 for a clean `exited_ok`; 1 for every other outcome, so cron sees even a
/// recorded failure or a leftover process.
fn exit_code_for(record: &ExportRecord) -> i32 {
    if record.status == Status::ExitedOk && record.safe_error_code.is_none() {
        0
    } else {
        1
    }
}

/// The record of an attempt that never started a launcher: `outcome` is its
/// status and code, `line` the runner-log line that names why. Returns the exit
/// code (always 1). If the record cannot be written the failure is logged
/// after `line`; it is not retried.
fn record_not_launched(
    settings: &CollectorSettings,
    task: &str,
    attempt: &str,
    outcome: (Status, Option<SafeErrorCode>),
    line: LogLine<'_>,
    log: &mut dyn FnMut(&LogLine<'_>),
) -> i32 {
    let (status, code) = outcome;
    let record =
        ExportRecord::not_launched(&settings.runner_instance_id, task, attempt, status, code);
    log(&line);
    if write_export(&settings.export_dir, &record).is_err() {
        log(&LogLine::ExportFailed { attempt });
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::store::{AccountMeta, State};
    use crate::task::collector::export::export_file_name;
    use crate::task::config::{self, TaskDef};
    use serde_json::Value;

    /// A temporary data root with a `Store`, a home, and an export directory
    /// that does not exist yet.
    struct Fx {
        _dir: tempfile::TempDir,
        store: Store,
        data_root: PathBuf,
        home: PathBuf,
        live_plugins: PathBuf,
        export_dir: PathBuf,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let data_root = dir.path().join("data");
            std::fs::create_dir_all(&data_root).unwrap();
            let home = dir.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            // Where the settings point `node` and the Geralt checkout; a test
            // that launches writes its stand-in `node` into `bin`.
            std::fs::create_dir_all(dir.path().join("bin")).unwrap();
            std::fs::create_dir_all(dir.path().join("repo")).unwrap();
            Fx {
                store: Store::new(&data_root),
                live_plugins: home.join(".claude").join("plugins"),
                export_dir: dir.path().join("export"),
                data_root,
                home,
                _dir: dir,
            }
        }

        fn ctx(&self) -> RunCtx<'_> {
            RunCtx {
                store: &self.store,
                home: &self.home,
                live_plugins: &self.live_plugins,
            }
        }

        fn root(&self) -> &Path {
            self._dir.path()
        }

        /// The stand-in `node`: absent until a test calls `write_node`.
        fn node_path(&self) -> PathBuf {
            self.root().join("bin").join("node")
        }

        fn settings(&self) -> CollectorSettings {
            CollectorSettings {
                node: self.node_path(),
                geralt_repo: self.root().join("repo"),
                export_dir: self.export_dir.clone(),
                runner_instance_id: "runner-test".into(),
            }
        }

        /// Install a stand-in for `node` that runs `body` as a POSIX shell script
        /// and returns its path. This is the only program a test in this module
        /// ever launches: never the real `node`, never Geralt's launcher, never
        /// `claude`. `PATH` is pinned so the script cannot find anything else.
        fn write_node(&self, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.node_path();
            std::fs::write(&path, format!("#!/bin/sh\nPATH=/usr/bin:/bin\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// A `Tasks` with the collector section set and the given tasks in it.
        fn tasks(&self, defs: Vec<(&str, TaskDef)>) -> Tasks {
            let mut t = Tasks {
                collector: Some(self.settings()),
                ..Tasks::default()
            };
            for (id, def) in defs {
                t.tasks.insert(id.to_string(), def);
            }
            t
        }

        fn run(&self, tasks: &Tasks, id: &str) -> (i32, Vec<String>) {
            let mut lines = Vec::new();
            let code = run_with_log(&self.ctx(), tasks, id, &mut |l| lines.push(l.render()));
            (code, lines)
        }

        fn write_state(&self, active: Option<&str>, accounts: &[&str]) {
            let mut state = State {
                active_alias: active.map(String::from),
                ..State::default()
            };
            for a in accounts {
                state
                    .accounts
                    .insert((*a).to_string(), AccountMeta::default());
            }
            self.store.save_state(&state).unwrap();
        }

        /// The export directory's files, sorted by name, each parsed.
        fn exports(&self) -> Vec<(String, Value)> {
            let Ok(rd) = std::fs::read_dir(&self.export_dir) else {
                return Vec::new();
            };
            let mut v: Vec<(String, Value)> = rd
                .map(|e| {
                    let e = e.unwrap();
                    let name = e.file_name().to_string_lossy().into_owned();
                    let body = std::fs::read(e.path()).unwrap();
                    (name, serde_json::from_slice(&body).unwrap())
                })
                .collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        }
    }

    fn collector_def(account: &str) -> TaskDef {
        TaskDef {
            kind: Kind::Collector,
            account: account.into(),
            times: vec!["06:00".into()],
            prompt: None,
            cwd: None,
            profile: None,
            model: None,
            collector: Some(CollectorTask {
                launcher_config: PathBuf::from("/etc/geralt/launcher.json"),
                wall_timeout_secs: 600,
                grace_ms: 5000,
            }),
            last_session_id: None,
            last_config_dir: None,
            last_run: None,
            last_status: None,
        }
    }

    fn plain_def(kind: Kind) -> TaskDef {
        TaskDef {
            kind,
            account: "work".into(),
            times: vec!["06:00".into()],
            prompt: None,
            cwd: None,
            profile: None,
            model: None,
            collector: None,
            last_session_id: None,
            last_config_dir: None,
            last_run: None,
            last_status: None,
        }
    }

    /// The one export of `fx`, as (file name, record).
    fn only_export(fx: &Fx) -> (String, Value) {
        let mut all = fx.exports();
        assert_eq!(all.len(), 1, "expected exactly one export file: {all:?}");
        all.remove(0)
    }

    fn assert_not_launched(rec: &Value, status: &str, code: Option<&str>) {
        assert_eq!(rec["status"], status);
        assert_eq!(rec["safeErrorCode"], code.map_or(Value::Null, Value::from));
        for k in ["sessionId", "exitCode", "startedAt", "endedAt"] {
            assert_eq!(rec[k], Value::Null, "{k} must be null");
        }
        assert_eq!(rec["schema"], "geralt.execution-record");
        assert_eq!(rec["version"], 1);
        assert_eq!(rec["recordRevision"], 1);
        assert_eq!(rec["runnerInstanceId"], "runner-test");
    }

    fn attempt_of(rec: &Value) -> String {
        rec["attemptId"].as_str().unwrap().to_string()
    }

    #[test]
    fn routes_to_collector_cases() {
        let fx = Fx::new();
        let with_section = fx.tasks(vec![
            ("c", collector_def("work")),
            ("p", plain_def(Kind::Prime)),
            ("t", plain_def(Kind::Task)),
        ]);
        assert!(routes_to_collector(&with_section, "c"));
        assert!(!routes_to_collector(&with_section, "p"));
        assert!(!routes_to_collector(&with_section, "t"));
        // Unknown id with a collector section: recorded as launch_failed.
        assert!(routes_to_collector(&with_section, "ghost"));

        // Unknown id without one: the plain "unknown task" error stays.
        let mut no_section = with_section.clone();
        no_section.collector = None;
        assert!(!routes_to_collector(&no_section, "ghost"));
        assert!(!routes_to_collector(&no_section, "p"));
        // A collector task whose section is missing still routes here, where
        // the missing section is reported.
        assert!(routes_to_collector(&no_section, "c"));
        assert!(!routes_to_collector(&Tasks::default(), "anything"));
    }

    #[test]
    fn unknown_task_writes_launch_failed_with_nulls() {
        let fx = Fx::new();
        let tasks = fx.tasks(vec![]);
        let (code, lines) = fx.run(&tasks, "ghost");
        assert_eq!(code, 1);
        let (name, rec) = only_export(&fx);
        assert_not_launched(&rec, "launch_failed", None);
        assert_eq!(rec["taskId"], "ghost");
        assert_eq!(rec["taskRevision"], Value::Null);
        for k in ["requestedModel", "effectiveModel", "effectivePolicyDigest"] {
            assert_eq!(rec[k], Value::Null);
        }
        let attempt = attempt_of(&rec);
        assert_eq!(name, export_file_name(&attempt, 1));
        assert_eq!(
            lines,
            [format!(
                "cc-loadout-collector prelaunch_failed attempt={attempt} reason=unknown_task"
            )]
        );
    }

    #[test]
    fn busy_writes_skipped_busy_lock_busy() {
        let fx = Fx::new();
        fx.write_state(None, &["work"]);
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let held = lock::acquire(&fx.store.lock_path()).unwrap();

        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (_, rec) = only_export(&fx);
        assert_not_launched(&rec, "skipped_busy", Some("lock_busy"));
        assert_eq!(rec["taskId"], "weekly");
        assert_eq!(
            lines,
            [format!(
                "cc-loadout-collector lock_busy attempt={}",
                attempt_of(&rec)
            )]
        );
        // The attempt did not take, release or disturb the other holder's lock.
        assert!(lock::try_acquire(&fx.store.lock_path()).unwrap().is_none());
        drop(held);
    }

    #[test]
    fn busy_does_not_wait() {
        let fx = Fx::new();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let _held = lock::acquire(&fx.store.lock_path()).unwrap();
        let start = std::time::Instant::now();
        let (code, _) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn unknown_account_writes_launch_failed() {
        let fx = Fx::new();
        fx.write_state(None, &["someone-else"]);
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (_, rec) = only_export(&fx);
        assert_not_launched(&rec, "launch_failed", None);
        assert_eq!(rec["taskId"], "weekly");
        assert_eq!(
            lines,
            [format!(
                "cc-loadout-collector prelaunch_failed attempt={} reason=unknown_account",
                attempt_of(&rec)
            )]
        );
        // No state file at all: no account either.
        let fx2 = Fx::new();
        let tasks2 = fx2.tasks(vec![("weekly", collector_def("work"))]);
        assert_eq!(fx2.run(&tasks2, "weekly").0, 1);
        assert_not_launched(&only_export(&fx2).1, "launch_failed", None);
    }

    #[test]
    fn state_unreadable_writes_launch_failed() {
        let fx = Fx::new();
        std::fs::write(fx.store.state_path(), b"{ not json").unwrap();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (_, rec) = only_export(&fx);
        assert_not_launched(&rec, "launch_failed", None);
        assert!(lines[0].ends_with("reason=state_unreadable"), "{lines:?}");
    }

    #[test]
    fn isolated_dir_failure_writes_launch_failed() {
        // The account exists and is not the active one, but it has no
        // credential snapshot, so its isolated config dir cannot be made.
        let fx = Fx::new();
        fx.write_state(Some("other"), &["work", "other"]);
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (_, rec) = only_export(&fx);
        assert_not_launched(&rec, "launch_failed", None);
        assert!(lines[0].ends_with("reason=isolated_dir"), "{lines:?}");
    }

    #[test]
    fn invalid_task_writes_launch_failed() {
        let fx = Fx::new();
        fx.write_state(None, &["work"]);
        // Not a collector at all.
        let tasks = fx.tasks(vec![("p", plain_def(Kind::Prime))]);
        let (code, lines) = fx.run(&tasks, "p");
        assert_eq!(code, 1);
        assert_not_launched(&only_export(&fx).1, "launch_failed", None);
        assert!(lines[0].ends_with("reason=invalid_task"), "{lines:?}");

        // A collector that fails its own validation (grace below the floor).
        let fx = Fx::new();
        fx.write_state(None, &["work"]);
        let mut bad = collector_def("work");
        bad.collector.as_mut().unwrap().grace_ms = 10;
        let tasks = fx.tasks(vec![("weekly", bad)]);
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        assert_not_launched(&only_export(&fx).1, "launch_failed", None);
        assert!(lines[0].ends_with("reason=invalid_task"), "{lines:?}");
    }

    #[test]
    fn lock_io_error_writes_launch_failed() {
        let fx = Fx::new();
        // A directory where the lock file belongs: not busy, broken.
        std::fs::create_dir_all(fx.store.lock_path()).unwrap();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (_, rec) = only_export(&fx);
        assert_not_launched(&rec, "launch_failed", None);
        assert_eq!(
            lines,
            [format!(
                "cc-loadout-collector prelaunch_failed attempt={} reason=lock_io",
                attempt_of(&rec)
            )]
        );
    }

    #[test]
    fn invalid_task_id_writes_no_export() {
        let fx = Fx::new();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let long = "a".repeat(201);
        for bad in ["-x", "", "a b", "a/b", ".hidden", "é", long.as_str()] {
            let (code, lines) = fx.run(&tasks, bad);
            assert_eq!(code, 1, "{bad:?}");
            assert_eq!(lines.len(), 1);
            assert!(
                lines[0].starts_with("cc-loadout-collector prelaunch_failed attempt=att-")
                    && lines[0].ends_with("reason=invalid_task_id"),
                "{lines:?}"
            );
            assert!(
                !lines[0].contains(bad) || bad.is_empty(),
                "id echoed in log"
            );
        }
        assert!(fx.exports().is_empty());
        assert!(
            !fx.export_dir.exists(),
            "no directory made for a non-record"
        );
    }

    #[test]
    fn missing_settings_writes_no_export() {
        let fx = Fx::new();
        let mut tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        tasks.collector = None;
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        assert_eq!(lines, ["cc-loadout-collector collector_unconfigured"]);
        assert!(fx.exports().is_empty());
        assert!(!fx.export_dir.exists());
    }

    #[test]
    fn invalid_settings_write_no_export() {
        let fx = Fx::new();
        let mut tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        tasks.collector.as_mut().unwrap().export_dir = PathBuf::from("relative/export");
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        assert_eq!(lines, ["cc-loadout-collector collector_settings_invalid"]);
        assert!(!fx.export_dir.exists());
        assert!(!Path::new("relative/export").exists());
    }

    #[test]
    fn attempt_id_is_generated_before_the_lock() {
        let fx = Fx::new();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let _held = lock::acquire(&fx.store.lock_path()).unwrap();
        let (code, lines) = fx.run(&tasks, "weekly");
        assert_eq!(code, 1);
        let (name, rec) = only_export(&fx);
        let attempt = attempt_of(&rec);
        assert!(
            attempt.starts_with("att-") && attempt.len() == 40,
            "{attempt}"
        );
        assert!(attempt[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'-'));
        assert_eq!(name, export_file_name(&attempt, 1));
        assert!(lines[0].contains(&format!("attempt={attempt}")));
    }

    #[test]
    fn every_attempt_gets_its_own_record() {
        let fx = Fx::new();
        let tasks = fx.tasks(vec![]);
        fx.run(&tasks, "ghost");
        fx.run(&tasks, "ghost");
        let all = fx.exports();
        assert_eq!(all.len(), 2);
        assert_ne!(attempt_of(&all[0].1), attempt_of(&all[1].1));
    }

    #[test]
    fn prelaunch_paths_leave_tasks_json_untouched() {
        // Each scenario: write tasks.json, run, and compare the bytes.
        type Scenario = (&'static str, Box<dyn Fn(&Fx) -> (Tasks, &'static str)>);
        let scenarios: Vec<Scenario> = vec![
            ("unknown task", Box::new(|fx| (fx.tasks(vec![]), "ghost"))),
            (
                "unknown account",
                Box::new(|fx| {
                    fx.write_state(None, &[]);
                    (fx.tasks(vec![("weekly", collector_def("work"))]), "weekly")
                }),
            ),
            (
                "state unreadable",
                Box::new(|fx| {
                    std::fs::write(fx.store.state_path(), b"{ not json").unwrap();
                    (fx.tasks(vec![("weekly", collector_def("work"))]), "weekly")
                }),
            ),
            (
                "isolated dir",
                Box::new(|fx| {
                    fx.write_state(Some("other"), &["work", "other"]);
                    (fx.tasks(vec![("weekly", collector_def("work"))]), "weekly")
                }),
            ),
            (
                "invalid task",
                Box::new(|fx| (fx.tasks(vec![("p", plain_def(Kind::Prime))]), "p")),
            ),
            (
                "lock io",
                Box::new(|fx| {
                    std::fs::create_dir_all(fx.store.lock_path()).unwrap();
                    (fx.tasks(vec![("weekly", collector_def("work"))]), "weekly")
                }),
            ),
            (
                "invalid task id",
                Box::new(|fx| (fx.tasks(vec![("weekly", collector_def("work"))]), "-x")),
            ),
            (
                "missing settings",
                Box::new(|fx| {
                    let mut t = fx.tasks(vec![("weekly", collector_def("work"))]);
                    t.collector = None;
                    (t, "weekly")
                }),
            ),
        ];
        for (name, build) in scenarios {
            let fx = Fx::new();
            let (tasks, id) = build(&fx);
            let path = config::tasks_path(&fx.data_root);
            config::save(&path, &tasks).unwrap();
            let before = std::fs::read(&path).unwrap();
            let (code, _) = fx.run(&tasks, id);
            assert_eq!(code, 1, "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name}: tasks.json");
        }

        // Busy needs the test to hold the lock while the attempt runs.
        let fx = Fx::new();
        fx.write_state(None, &["work"]);
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let path = config::tasks_path(&fx.data_root);
        config::save(&path, &tasks).unwrap();
        let before = std::fs::read(&path).unwrap();
        let _held = lock::acquire(&fx.store.lock_path()).unwrap();
        assert_eq!(fx.run(&tasks, "weekly").0, 1);
        assert_eq!(std::fs::read(&path).unwrap(), before, "busy: tasks.json");
    }

    #[test]
    fn prelaunch_paths_leave_state_json_untouched() {
        let fx = Fx::new();
        fx.write_state(None, &["work"]);
        let before = std::fs::read(fx.store.state_path()).unwrap();
        let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
        let _held = lock::acquire(&fx.store.lock_path()).unwrap();
        fx.run(&tasks, "weekly");
        fx.run(&tasks, "ghost");
        assert_eq!(std::fs::read(fx.store.state_path()).unwrap(), before);
    }

    #[test]
    fn an_unwritable_export_is_logged_and_the_run_still_exits_1() {
        let fx = Fx::new();
        // The export "directory" is a regular file: the record cannot be placed.
        std::fs::write(&fx.export_dir, b"not a directory").unwrap();
        let tasks = fx.tasks(vec![]);
        let (code, lines) = fx.run(&tasks, "ghost");
        assert_eq!(code, 1);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].ends_with("reason=unknown_task"));
        assert!(lines[1].starts_with("cc-loadout-collector export_write_failed attempt=att-"));
        // The same attempt id in both lines.
        let attempt = |l: &str| {
            l.split("attempt=")
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap()
                .to_string()
        };
        assert_eq!(attempt(&lines[0]), attempt(&lines[1]));
    }

    fn finished(status: Status, code: Option<SafeErrorCode>) -> ExportRecord {
        ExportRecord::not_launched("runner-test", "weekly", "att-fin", status, code)
    }

    #[test]
    fn exit_code_is_zero_only_for_a_clean_exited_ok() {
        assert_eq!(exit_code_for(&finished(Status::ExitedOk, None)), 0);
        for (status, code) in [
            (Status::ExitedOk, Some(SafeErrorCode::ProcessTreeResidual)),
            (Status::ExitedError, None),
            (Status::ExitedError, Some(SafeErrorCode::SessionError)),
            (Status::TimedOut, None),
            (Status::Killed, None),
            (Status::SkippedBusy, Some(SafeErrorCode::LockBusy)),
            (Status::LaunchFailed, None),
        ] {
            assert_eq!(
                exit_code_for(&finished(status, code)),
                1,
                "{status:?}/{code:?}"
            );
        }
    }

    #[test]
    fn finish_writes_the_record_logs_it_and_maps_the_exit_code() {
        let fx = Fx::new();
        let settings = fx.settings();
        let mut lines = Vec::new();
        let code = finish(&settings, &finished(Status::ExitedOk, None), &mut |l| {
            lines.push(l.render())
        });
        assert_eq!(code, 0);
        assert_eq!(
            lines,
            ["cc-loadout-collector attempt_recorded attempt=att-fin status=exited_ok safe_error=none"]
        );
        let (name, rec) = only_export(&fx);
        assert_eq!(name, "att-fin.1.json");
        assert_eq!(rec["status"], "exited_ok");

        // A second record for the same attempt is refused, never overwritten:
        // that is an export failure, and the first file stays as it was.
        let before = std::fs::read(fx.export_dir.join("att-fin.1.json")).unwrap();
        let mut lines = Vec::new();
        let code = finish(&settings, &finished(Status::ExitedError, None), &mut |l| {
            lines.push(l.render())
        });
        assert_eq!(code, 1);
        assert_eq!(
            lines,
            ["cc-loadout-collector export_write_failed attempt=att-fin"]
        );
        assert_eq!(
            std::fs::read(fx.export_dir.join("att-fin.1.json")).unwrap(),
            before
        );
    }

    #[test]
    fn finish_with_a_failed_run_logs_status_and_code_and_exits_1() {
        let fx = Fx::new();
        let mut lines = Vec::new();
        let code = finish(
            &fx.settings(),
            &finished(Status::ExitedOk, Some(SafeErrorCode::ProcessTreeResidual)),
            &mut |l| lines.push(l.render()),
        );
        assert_eq!(code, 1);
        assert_eq!(
            lines,
            ["cc-loadout-collector attempt_recorded attempt=att-fin status=exited_ok safe_error=process_tree_residual"]
        );
    }

    /// Tests that reach step 8 run a stand-in `node` (see `Fx::write_node`), and
    /// step 6 only lets a run get there on Linux.
    #[cfg(target_os = "linux")]
    mod launched {
        use super::*;
        use crate::account::creds;
        use crate::task::collector::outcome::STDOUT_CAP_BYTES;
        use crate::task::collector::test_support::KillGrandchild;

        fn digest() -> String {
            format!("sha256:{}", "a".repeat(64))
        }

        /// The JSON object of a valid outcome line, without its newline.
        fn outcome_json() -> String {
            format!(
                "{{\"schema\":\"geralt.launch-outcome\",\"version\":1,\"requestedModel\":\"m1\",\"effectiveModel\":\"m2\",\"effectivePolicyDigest\":\"{}\"}}",
                digest()
            )
        }

        /// A shell command that prints a valid outcome line.
        fn print_outcome() -> String {
            format!("printf '%s\\n' '{}'", outcome_json())
        }

        /// A shell command that records argv (one word per line) and the config
        /// dir the stand-in was given into `dump`.
        fn dump_to(dump: &Path) -> String {
            format!(
                "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{d}'\nprintf 'cfg=%s\\n' \"${{CLAUDE_CONFIG_DIR-unset}}\" >> '{d}'",
                d = dump.display()
            )
        }

        fn live_fx() -> (Fx, Tasks) {
            let fx = Fx::new();
            fx.write_state(Some("work"), &["work"]);
            let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
            (fx, tasks)
        }

        /// The record shape every launched attempt shares.
        fn assert_launched(rec: &Value) {
            assert_eq!(rec["schema"], "geralt.execution-record");
            assert_eq!(rec["version"], 1);
            assert_eq!(rec["recordRevision"], 1);
            assert_eq!(rec["runnerInstanceId"], "runner-test");
            assert_eq!(rec["taskId"], "weekly");
            assert_eq!(rec["taskRevision"], Value::Null);
            let session = rec["sessionId"].as_str().unwrap();
            assert_eq!(session.len(), 36, "a UUID: {session}");
            let (started, ended) = (
                rec["startedAt"].as_str().unwrap(),
                rec["endedAt"].as_str().unwrap(),
            );
            for t in [started, ended] {
                assert_eq!(t.len(), 24, "{t}");
                assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T', "{t}");
            }
            // Q3 rule 5: the same fixed-width format, so text order is time order.
            assert!(started <= ended, "{started} .. {ended}");
        }

        #[test]
        fn a_clean_launcher_exit_is_an_exited_ok_record_with_the_outcome_models() {
            let (fx, tasks) = live_fx();
            let dump = fx.root().join("dump");
            fx.write_node(&format!("{}\n{}", dump_to(&dump), print_outcome()));

            let (code, lines) = fx.run(&tasks, "weekly");
            assert_eq!(code, 0);
            let (name, rec) = only_export(&fx);
            assert_launched(&rec);
            assert_eq!(rec["status"], "exited_ok");
            assert_eq!(rec["safeErrorCode"], Value::Null);
            assert_eq!(rec["exitCode"], 0);
            assert_eq!(rec["requestedModel"], "m1");
            assert_eq!(rec["effectiveModel"], "m2");
            assert_eq!(rec["effectivePolicyDigest"], digest());

            let attempt = attempt_of(&rec);
            assert_eq!(name, export_file_name(&attempt, 1));
            assert_eq!(
                lines,
                [
                    format!("cc-loadout-collector attempt_started attempt={attempt}"),
                    format!(
                        "cc-loadout-collector attempt_recorded attempt={attempt} status=exited_ok safe_error=none"
                    ),
                ]
            );

            // The launcher got this attempt's and this session's names and the
            // contract argv.
            let dumped = std::fs::read_to_string(&dump).unwrap();
            let mut words: Vec<&str> = dumped.lines().collect();
            let cfg_line = words.pop().unwrap();
            let main_ts = fx.root().join("repo/src/launcher/main.ts");
            assert_eq!(
                words,
                [
                    "--import",
                    "tsx",
                    main_ts.to_str().unwrap(),
                    "run",
                    "--config",
                    "/etc/geralt/launcher.json",
                    "--attempt-ref",
                    attempt.as_str(),
                    "--session-id",
                    rec["sessionId"].as_str().unwrap(),
                ]
            );

            // The live account runs in the user's own config dir: the runner leaves
            // CLAUDE_CONFIG_DIR exactly as it inherited it, whatever that is, and
            // never points it at an isolated dir under the fixture. (The messages
            // carry no values: the variable belongs to the machine running the test.)
            let inherited = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map_or_else(|| "unset".to_string(), |v| v.to_string_lossy().into_owned());
            assert!(
                cfg_line == format!("cfg={inherited}"),
                "the live account must leave CLAUDE_CONFIG_DIR as inherited"
            );
            assert!(
                !cfg_line.contains(fx.root().to_str().unwrap()),
                "the live account must not run in an isolated config dir"
            );
        }

        #[test]
        fn every_launcher_exit_lands_in_its_row_of_the_table() {
            // (launcher exit, status, safeErrorCode, record exitCode, runner exit)
            let rows = [
                (0, "exited_ok", None, Some(0), 0),
                (1, "exited_error", Some("launcher_error"), Some(1), 1),
                (2, "launch_failed", Some("config_invalid"), None, 1),
                (7, "launch_failed", Some("auth_required"), None, 1),
                (21, "exited_error", Some("turn_limit"), Some(21), 1),
                (22, "exited_error", Some("session_error"), Some(22), 1),
                (30, "exited_error", None, Some(30), 1),
                (99, "exited_error", None, Some(99), 1),
            ];
            for (launcher_exit, status, safe, exit_code, runner_exit) in rows {
                let (fx, tasks) = live_fx();
                fx.write_node(&format!("exit {launcher_exit}"));
                let (code, lines) = fx.run(&tasks, "weekly");
                let ctx = format!("launcher exit {launcher_exit}");
                assert_eq!(code, runner_exit, "{ctx}");
                let (_, rec) = only_export(&fx);
                assert_launched(&rec);
                assert_eq!(rec["status"], status, "{ctx}");
                assert_eq!(
                    rec["safeErrorCode"],
                    safe.map_or(Value::Null, Value::from),
                    "{ctx}"
                );
                assert_eq!(
                    rec["exitCode"],
                    exit_code.map_or(Value::Null, Value::from),
                    "{ctx}"
                );
                // No outcome line was printed: no model, no digest.
                for k in ["requestedModel", "effectiveModel", "effectivePolicyDigest"] {
                    assert_eq!(rec[k], Value::Null, "{ctx}: {k}");
                }
                assert!(lines[0].starts_with("cc-loadout-collector attempt_started "));
                assert!(lines.last().unwrap().contains("attempt_recorded"), "{ctx}");
            }
        }

        #[test]
        fn after_a_failed_start_only_the_requested_model_is_believed() {
            // Exit 8 with a complete outcome line: the effective model and the
            // policy digest are dropped, the requested model stays.
            let (fx, tasks) = live_fx();
            fx.write_node(&format!("{}\nexit 8", print_outcome()));
            assert_eq!(fx.run(&tasks, "weekly").0, 1);
            let (_, rec) = only_export(&fx);
            assert_eq!(rec["status"], "launch_failed");
            assert_eq!(rec["safeErrorCode"], "preflight_failed");
            assert_eq!(rec["exitCode"], Value::Null);
            assert_eq!(rec["requestedModel"], "m1");
            assert_eq!(rec["effectiveModel"], Value::Null);
            assert_eq!(rec["effectivePolicyDigest"], Value::Null);
        }

        #[test]
        fn a_launcher_ended_by_a_signal_is_an_exited_error_without_an_exit_code() {
            let (fx, tasks) = live_fx();
            fx.write_node("kill -KILL $$");
            let (code, _) = fx.run(&tasks, "weekly");
            assert_eq!(code, 1);
            let (_, rec) = only_export(&fx);
            assert_launched(&rec);
            assert_eq!(rec["status"], "exited_error");
            assert_eq!(rec["safeErrorCode"], Value::Null);
            assert_eq!(rec["exitCode"], Value::Null);
        }

        #[test]
        fn stdout_that_is_not_one_valid_outcome_line_gives_no_models() {
            // The oversized case is only a test of the `stdout_truncated` guard if
            // the kept prefix would parse by itself: a valid line with no newline,
            // padded with spaces (which JSON allows after a value) to the cap. Prove
            // that premise here, so the case cannot pass for the wrong reason.
            let mut prefix = outcome_json().into_bytes();
            prefix.resize(STDOUT_CAP_BYTES, b' ');
            assert!(
                parse_outcome_line(&prefix).is_some(),
                "the first {STDOUT_CAP_BYTES} bytes of the oversized case must parse alone"
            );

            let cases: [(&str, String); 3] = [
                ("garbage", "echo 'not json'".to_string()),
                (
                    "the line plus more output",
                    format!("{}\necho 'and more'", print_outcome()),
                ),
                (
                    // The valid line plus 8000 bytes of padding run past the cap, and
                    // the kept prefix parses, so only the guard can reject it.
                    "an oversized output whose first 4096 bytes parse",
                    format!(
                        "printf '%s' '{}'\nhead -c 8000 /dev/zero | tr '\\0' ' '",
                        outcome_json()
                    ),
                ),
            ];
            for (name, script) in cases {
                let (fx, tasks) = live_fx();
                fx.write_node(&format!("{script}\nexit 0"));
                assert_eq!(fx.run(&tasks, "weekly").0, 0, "{name}");
                let (_, rec) = only_export(&fx);
                assert_eq!(rec["status"], "exited_ok", "{name}");
                for k in ["requestedModel", "effectiveModel", "effectivePolicyDigest"] {
                    assert_eq!(rec[k], Value::Null, "{name}: {k}");
                }
            }
        }

        #[test]
        fn a_megabyte_of_output_does_not_stall_the_launcher_or_reach_the_record() {
            let (fx, tasks) = live_fx();
            fx.write_node("head -c 1048576 /dev/zero\nhead -c 1048576 /dev/zero >&2\nexit 0");
            let (code, lines) = fx.run(&tasks, "weekly");
            assert_eq!(code, 0);
            let (_, rec) = only_export(&fx);
            assert_eq!(rec["status"], "exited_ok");
            // The megabyte of zeros on stderr has no newline, so it is one line, and
            // that line is counted, never printed: the log is exactly the start, the
            // one drop count, and the record.
            let attempt = attempt_of(&rec);
            assert_eq!(
                lines,
                [
                    format!("cc-loadout-collector attempt_started attempt={attempt}"),
                    format!("cc-loadout-collector launcher_stderr_dropped attempt={attempt} lines=1"),
                    format!(
                        "cc-loadout-collector attempt_recorded attempt={attempt} status=exited_ok safe_error=none"
                    ),
                ]
            );
            let body =
                std::fs::read_to_string(fx.export_dir.join(export_file_name(&attempt_of(&rec), 1)))
                    .unwrap();
            assert!(body.len() < 1024, "{} bytes", body.len());
        }

        #[test]
        fn a_missing_launcher_program_is_spawn_enoent_with_a_session_and_times() {
            let (fx, tasks) = live_fx();
            // No `node` written: the program does not exist.
            let (code, lines) = fx.run(&tasks, "weekly");
            assert_eq!(code, 1);
            let (_, rec) = only_export(&fx);
            assert_launched(&rec);
            assert_eq!(rec["status"], "launch_failed");
            assert_eq!(rec["safeErrorCode"], "spawn_enoent");
            assert_eq!(rec["exitCode"], Value::Null);
            // Nothing ran, so the instant it was tried is both its start and end.
            assert_eq!(rec["startedAt"], rec["endedAt"]);
            // No process was started, so there is no "started" line either.
            assert_eq!(
                lines,
                [format!(
                    "cc-loadout-collector attempt_recorded attempt={} status=launch_failed safe_error=spawn_enoent",
                    attempt_of(&rec)
                )]
            );
        }

        #[test]
        fn a_missing_geralt_checkout_is_also_spawn_enoent() {
            // The working directory is part of starting the launcher: the OS
            // reports it with the same error as a missing program.
            let (fx, tasks) = live_fx();
            fx.write_node("exit 0");
            let mut tasks = tasks;
            tasks.collector.as_mut().unwrap().geralt_repo = fx.root().join("no-such-repo");
            assert_eq!(fx.run(&tasks, "weekly").0, 1);
            let (_, rec) = only_export(&fx);
            assert_eq!(rec["status"], "launch_failed");
            assert_eq!(rec["safeErrorCode"], "spawn_enoent");
        }

        #[test]
        fn a_launcher_program_that_cannot_run_is_spawn_failed() {
            use std::os::unix::fs::PermissionsExt;
            let (fx, tasks) = live_fx();
            let node = fx.write_node("exit 0");
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o644)).unwrap();
            // With no execute bit at all even root cannot run it.
            let (code, _) = fx.run(&tasks, "weekly");
            assert_eq!(code, 1);
            let (_, rec) = only_export(&fx);
            assert_launched(&rec);
            assert_eq!(rec["status"], "launch_failed");
            assert_eq!(rec["safeErrorCode"], "spawn_failed");
            assert_eq!(rec["exitCode"], Value::Null);
        }

        #[test]
        fn stderr_that_is_not_forwarded_is_counted_in_the_runner_log() {
            let (fx, tasks) = live_fx();
            fx.write_node("echo noise-1 >&2\necho noise-2 >&2\necho noise-3 >&2\nexit 0");
            let (code, lines) = fx.run(&tasks, "weekly");
            assert_eq!(code, 0);
            let attempt = attempt_of(&only_export(&fx).1);
            assert_eq!(
                lines,
                [
                    format!("cc-loadout-collector attempt_started attempt={attempt}"),
                    format!(
                        "cc-loadout-collector launcher_stderr_dropped attempt={attempt} lines=3"
                    ),
                    format!(
                        "cc-loadout-collector attempt_recorded attempt={attempt} status=exited_ok safe_error=none"
                    ),
                ]
            );
            assert!(lines.iter().all(|l| !l.contains("noise")));
        }

        #[test]
        fn an_isolated_account_gets_its_config_dir_and_its_rotated_credentials_are_kept() {
            let fx = Fx::new();
            fx.write_state(Some("other"), &["work", "other"]);
            fx.store.ensure_account_dir("work").unwrap();
            let before =
                serde_json::json!({"claudeAiOauth":{"accessToken":"A","refreshToken":"R"}});
            creds::write_credentials(&fx.store.credentials_snapshot("work"), &before).unwrap();
            std::fs::create_dir_all(&fx.live_plugins).unwrap();
            let dump = fx.root().join("dump");
            fx.write_node(&format!(
                "{}\nprintf '%s' '{{\"claudeAiOauth\":{{\"accessToken\":\"ROTATED\",\"refreshToken\":\"NEW\"}}}}' > \"$CLAUDE_CONFIG_DIR/.credentials.json\"",
                dump_to(&dump)
            ));
            let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);

            let (code, _) = fx.run(&tasks, "weekly");
            assert_eq!(code, 0);
            assert_eq!(only_export(&fx).1["status"], "exited_ok");

            let cfg = fx.store.account_dir("work").join("run").join("cfg");
            let dumped = std::fs::read_to_string(&dump).unwrap();
            assert_eq!(
                dumped.lines().last().unwrap(),
                format!("cfg={}", cfg.display())
            );
            let after = creds::read_credentials(&fx.store.credentials_snapshot("work")).unwrap();
            assert_eq!(after["claudeAiOauth"]["accessToken"], "ROTATED");
            assert_eq!(after["claudeAiOauth"]["refreshToken"], "NEW");
        }

        #[test]
        fn rotated_credentials_are_kept_even_when_the_launcher_fails() {
            let fx = Fx::new();
            fx.write_state(Some("other"), &["work", "other"]);
            fx.store.ensure_account_dir("work").unwrap();
            let before =
                serde_json::json!({"claudeAiOauth":{"accessToken":"A","refreshToken":"R"}});
            creds::write_credentials(&fx.store.credentials_snapshot("work"), &before).unwrap();
            std::fs::create_dir_all(&fx.live_plugins).unwrap();
            fx.write_node(
                "printf '%s' '{\"claudeAiOauth\":{\"accessToken\":\"ROTATED\",\"refreshToken\":\"NEW\"}}' > \"$CLAUDE_CONFIG_DIR/.credentials.json\"\nexit 22",
            );
            let tasks = fx.tasks(vec![("weekly", collector_def("work"))]);
            assert_eq!(fx.run(&tasks, "weekly").0, 1);
            assert_eq!(only_export(&fx).1["status"], "exited_error");
            let after = creds::read_credentials(&fx.store.credentials_snapshot("work")).unwrap();
            assert_eq!(after["claudeAiOauth"]["accessToken"], "ROTATED");
        }

        #[test]
        fn a_launched_run_leaves_tasks_json_and_state_json_untouched() {
            let (fx, tasks) = live_fx();
            fx.write_node(&format!("{}\nexit 0", print_outcome()));
            let tasks_file = config::tasks_path(&fx.data_root);
            config::save(&tasks_file, &tasks).unwrap();
            let tasks_before = std::fs::read(&tasks_file).unwrap();
            let state_before = std::fs::read(fx.store.state_path()).unwrap();

            assert_eq!(fx.run(&tasks, "weekly").0, 0);

            assert_eq!(std::fs::read(&tasks_file).unwrap(), tasks_before);
            assert_eq!(std::fs::read(fx.store.state_path()).unwrap(), state_before);
        }

        #[test]
        fn a_grandchild_holding_the_pipes_does_not_hold_the_run() {
            let (fx, tasks) = live_fx();
            let pid_file = fx.root().join("grandchild.pid");
            let _cleanup = KillGrandchild(pid_file.clone());
            // The launcher ends at once; the grandchild keeps both pipes open.
            fx.write_node(&format!(
                "sleep 20 &\necho $! > '{}'\nexit 0",
                pid_file.display()
            ));
            let start = std::time::Instant::now();
            let (code, _) = fx.run(&tasks, "weekly");
            let took = start.elapsed();
            assert_eq!(code, 0);
            assert_eq!(only_export(&fx).1["status"], "exited_ok");
            assert!(took < Duration::from_secs(10), "the run took {took:?}");
        }
    }
}
