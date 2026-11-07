//! The collector run path behind `cc-loadout task run <id>` for a
//! `kind: collector` task (and for an unknown id when a collector is
//! configured). Unlike a plain task it never blocks on the cc-loadout lock,
//! never touches `tasks.json` or `state.json` for bookkeeping, and reports
//! every attempt as exactly one export record for Geralt's reporter. Only a
//! run that cannot even tell where to write that record ends with a log line
//! alone.

use std::path::{Path, PathBuf};

use crate::account::store::Store;
use crate::task::collector::export::{
    new_attempt_id, write_export, ExportRecord, SafeErrorCode, Status,
};
use crate::task::collector::log::{LogLine, PrelaunchReason};
use crate::task::config::{is_collector_id, CollectorSettings, CollectorTask, Kind, Tasks};
use crate::task::run::{run_location, RunLocation};
use crate::task::sandbox;
use crate::util::lock;

/// Where a collector run finds the things it shares with every other cc-loadout
/// command.
pub struct RunCtx<'a> {
    pub store: &'a Store,
    // Part of the run context every task path gets; a collector run never
    // writes `tasks.json` (its export record is the only bookkeeping), so
    // nothing reads it yet.
    #[allow(dead_code)]
    pub data_root: &'a Path,
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
// Read by the launch code of Task 4.
#[allow(dead_code)]
struct Launch<'a> {
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
            settings,
            task,
            account: &def.account,
            attempt: &attempt,
            session: &session,
            cfg_dir,
        },
    );

    // 9. Write the record; the lock is released when this function returns.
    finish(settings, &record, log)
}

/// Step 8: start the launcher and supervise it until the attempt is over, then
/// describe how it ended. Task 4 fills this in; nothing before it reaches it in
/// a test.
fn launch_and_watch(_ctx: &RunCtx<'_>, _launch: &Launch<'_>) -> ExportRecord {
    unimplemented!("Task 4")
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
                data_root: &self.data_root,
                home: &self.home,
                live_plugins: &self.live_plugins,
            }
        }

        fn settings(&self) -> CollectorSettings {
            CollectorSettings {
                node: PathBuf::from("/opt/node/bin/node"),
                geralt_repo: PathBuf::from("/opt/geralt"),
                export_dir: self.export_dir.clone(),
                runner_instance_id: "runner-test".into(),
            }
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
}
