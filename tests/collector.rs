//! The collector run path end to end, one test group per clause (1–9) of the
//! cc-loadout patch contract v2. Every test runs the real `cc-loadout` binary,
//! each invocation in a process of its own, so these are the tests that reach
//! the real child subreaper, the reaping of orphans and the real SIGTERM/SIGINT
//! handler, which the unit tests cannot (they share one process).
//!
//! What runs and what never does:
//! - `node` is always `tests/fixtures/collector/stub-launcher.sh`, a bash stub
//!   that follows a plan the test writes; never the real node, never Geralt's
//!   launcher. `geralt_repo` is a temp dir.
//! - Every `cc-loadout` runs through [`common::cmd`] (temp `HOME` and
//!   `XDG_DATA_HOME`, fake `crontab`) with `PATH` pinned to
//!   `<fake claude dir>:<fake crontab dir>:/usr/bin:/bin`, so a `claude` it
//!   might start is the fake one, never the real one.
//! - Every process the stub starts reports its pid and start time; a
//!   [`PidGuard`] kills exactly those (start time still matching) when a test
//!   ends, however it ends, and a [`ChildGuard`] does the same for a
//!   `cc-loadout` the test spawned.

mod common;

use common::{cmd, write_login};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// The only `node` any test here runs.
const STUB: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/collector/stub-launcher.sh"
);
/// Longest a single `task run` may take before the test gives up on it.
const RUN_LIMIT: Duration = Duration::from_secs(60);
/// Defaults of a collector task here, unless a test says otherwise.
const WALL: u64 = 30;
const GRACE: u64 = 3000;

const REQUESTED: &str = "claude-requested-model";
const EFFECTIVE: &str = "claude-effective-model";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A valid launcher outcome line with all three values set.
fn outcome_line() -> String {
    json!({
        "schema": "geralt.launch-outcome",
        "version": 1,
        "requestedModel": REQUESTED,
        "effectiveModel": EFFECTIVE,
        "effectivePolicyDigest": DIGEST,
    })
    .to_string()
}

/// `s` as one bash single-quoted word.
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A stub plan: `KEY='value'` lines.
fn plan(kv: &[(&str, &str)]) -> String {
    kv.iter()
        .map(|(k, v)| format!("{k}={}\n", shq(v)))
        .collect()
}

/// The sandboxed `cc-loadout`, as a plain `std::process::Command`, so it can be
/// spawned without waiting. Built from [`common::cmd`] — never a second
/// constructor — by copying its program, arguments, environment and directory.
fn to_std(c: &assert_cmd::Command) -> Command {
    let mut s = Command::new(c.get_program());
    s.args(c.get_args());
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => s.env(k, v),
            None => s.env_remove(k),
        };
    }
    if let Some(d) = c.get_current_dir() {
        s.current_dir(d);
    }
    s
}

/// `(state, starttime)` of `pid` from `/proc/<pid>/stat`, counted after the
/// parenthesised command name.
fn stat(pid: i32) -> Option<(char, u64)> {
    let line = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &line[line.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    Some((
        fields.first()?.chars().next()?,
        fields.get(19)?.parse().ok()?,
    ))
}

/// Whether `pid` is still the live process that started at `start`.
fn alive(pid: i32, start: u64) -> bool {
    matches!(stat(pid), Some((state, st)) if st == start && !matches!(state, 'Z' | 'X' | 'x'))
}

/// Every process in `pids` is gone: no `/proc` entry, a zombie, or a different
/// process under the same pid.
fn gone(pids: &[(i32, u64)]) -> bool {
    pids.iter().all(|&(pid, st)| !alive(pid, st))
}

/// Kills, on drop, every process a stub of this world reported whose start time
/// still matches; nothing else. Reads the report at drop time, so processes
/// started after the guard was made are covered too.
struct PidGuard(PathBuf);

impl Drop for PidGuard {
    fn drop(&mut self) {
        for (pid, st) in read_pids(&self.0) {
            if alive(pid, st) {
                let _ = Command::new("/bin/kill")
                    .args(["-KILL", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

fn read_pids(file: &Path) -> Vec<(i32, u64)> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (p, s) = l.split_once(' ')?;
            Some((p.parse().ok()?, s.trim().parse().ok()?))
        })
        .collect()
}

/// A `cc-loadout` the test spawned: SIGKILLed and reaped on drop if it is still
/// running.
struct ChildGuard(Child);

impl ChildGuard {
    fn pid(&self) -> i32 {
        i32::try_from(self.0.id()).unwrap()
    }

    /// Wait up to `limit` for it to end.
    fn wait_for(&mut self, limit: Duration) -> ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "cc-loadout still running after {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Send `sig` (a `kill` signal name) to the `cc-loadout` the test spawned.
fn signal_child(child: &ChildGuard, sig: &str) {
    let ok = Command::new("/bin/kill")
        .args([&format!("-{sig}"), &child.pid().to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -{sig} failed");
}

/// Names of the output files of `run`, unique in this test process.
static RUN_SEQ: AtomicUsize = AtomicUsize::new(0);

/// One sandbox: a home with the `work` account, a data root whose `tasks.json`
/// has the collector section, the stub as `node`, and a fake `claude`.
struct World {
    home: TempDir,
    data: TempDir,
    /// The fake `claude` (and its log) and the stub plans.
    bin: TempDir,
    /// `geralt_repo`: the launcher's working directory.
    repo: TempDir,
    export: PathBuf,
    /// The stub's `DUMP_DIR`.
    dump: PathBuf,
}

impl World {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let w = World {
            export: data.path().join("export"),
            dump: data.path().join("dump"),
            home: tempfile::tempdir().unwrap(),
            data,
            bin: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(&w.dump).unwrap();
        let claude = w.bin.path().join("claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
                shq(&w.claude_log().display().to_string())
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_login(w.home.path(), "work@x");
        w.cc().args(["account", "add", "work"]).assert().success();
        w.set_node(Path::new(STUB));
        w
    }

    fn claude_log(&self) -> PathBuf {
        self.bin.path().join("claude.log")
    }

    /// The sandboxed `cc-loadout` with `PATH` pinned: fake `claude`, fake
    /// `crontab`, the system dirs, nothing else.
    fn cc(&self) -> assert_cmd::Command {
        let mut c = cmd(self.home.path(), self.data.path());
        c.env(
            "PATH",
            format!(
                "{}:{}:/usr/bin:/bin",
                self.bin.path().display(),
                self.data.path().join(".fakebin").display()
            ),
        );
        c
    }

    /// Save the collector section with `node` as the launcher's interpreter.
    fn set_node(&self, node: &Path) {
        self.cc()
            .arg("collector")
            .arg("set")
            .arg("--node")
            .arg(node)
            .arg("--geralt-repo")
            .arg(self.repo.path())
            .arg("--export-dir")
            .arg(&self.export)
            .args(["--runner-instance-id", "runner-test"])
            .assert()
            .success();
    }

    fn tasks_path(&self) -> PathBuf {
        self.data.path().join("cc-loadout").join("tasks.json")
    }

    fn plan_path(&self, id: &str) -> PathBuf {
        self.bin.path().join(format!("{id}.plan"))
    }

    fn guard(&self) -> PidGuard {
        PidGuard(self.dump.join("pids"))
    }

    /// Add collector task `id` straight to `tasks.json` (no `task add`, so no
    /// crontab at all), with `plan` as the stub plan its launcher config names.
    fn task(&self, id: &str, plan: &str, wall_secs: u64, grace_ms: u64) {
        let plan_path = self.plan_path(id);
        std::fs::write(
            &plan_path,
            format!("DUMP_DIR={}\n{plan}", shq(&self.dump.display().to_string())),
        )
        .unwrap();
        let path = self.tasks_path();
        let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["version"] = json!(2);
        v["tasks"][id] = json!({
            "kind": "collector",
            "account": "work",
            "times": ["07:00"],
            "collector": {
                "launcher_config": plan_path,
                "wall_timeout_secs": wall_secs,
                "grace_ms": grace_ms,
            },
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    }

    fn run_command(&self, id: &str, env: &[(&str, &str)]) -> Command {
        let mut c = to_std(&self.cc());
        c.args(["task", "run", id, "--quiet"]).stdin(Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    /// `task run <id> --quiet`, waited for; panics after [`RUN_LIMIT`].
    fn run(&self, id: &str) -> Output {
        self.run_env(id, &[])
    }

    /// [`World::run`] with extra environment for `cc-loadout`.
    fn run_env(&self, id: &str, env: &[(&str, &str)]) -> Output {
        // Files, not pipes: the test never has to drain anything to make
        // progress, and nothing can hold them open past the run.
        let n = RUN_SEQ.fetch_add(1, Ordering::SeqCst);
        let out_dir = self.data.path().join(".out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let (out, err) = (
            out_dir.join(format!("{n}.out")),
            out_dir.join(format!("{n}.err")),
        );
        let child = self
            .run_command(id, env)
            .stdout(std::fs::File::create(&out).unwrap())
            .stderr(std::fs::File::create(&err).unwrap())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);
        let status = child.wait_for(RUN_LIMIT);
        Output {
            status,
            stdout: std::fs::read(&out).unwrap(),
            stderr: std::fs::read(&err).unwrap(),
        }
    }

    /// `task run <id> --quiet`, not waited for. Its output is discarded.
    fn spawn(&self, id: &str) -> Child {
        self.run_command(id, &[])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Every record in the export directory (file name, content), skipping
    /// dot files; empty when the directory does not exist.
    fn exports_named(&self) -> Vec<(String, Value)> {
        let Ok(dir) = std::fs::read_dir(&self.export) else {
            return Vec::new();
        };
        let mut v: Vec<(String, Value)> = dir
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| !n.starts_with('.'))
            .map(|n| {
                let body = std::fs::read(self.export.join(&n)).unwrap();
                (n, serde_json::from_slice(&body).unwrap())
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    fn exports(&self) -> Vec<Value> {
        self.exports_named().into_iter().map(|(_, v)| v).collect()
    }

    /// The one record of a world that has run once.
    fn only_export(&self) -> Value {
        let mut e = self.exports();
        assert_eq!(e.len(), 1, "expected exactly one export: {e:?}");
        e.remove(0)
    }

    /// The record of `task_id` (exactly one).
    fn export_of(&self, task_id: &str) -> Value {
        let mut e: Vec<Value> = self
            .exports()
            .into_iter()
            .filter(|v| v["taskId"] == task_id)
            .collect();
        assert_eq!(e.len(), 1, "expected one export of {task_id}: {e:?}");
        e.remove(0)
    }

    /// Pid and start time of every process the stubs of this world started.
    fn pids(&self) -> Vec<(i32, u64)> {
        read_pids(&self.dump.join("pids"))
    }

    /// The dump directories, one per stub run.
    fn runs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&self.dump)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("run-"))
            })
            .collect();
        v.sort();
        v
    }

    /// The pid of the stub that wrote dump directory `run`.
    fn stub_pid(run: &Path) -> i32 {
        run.file_name().unwrap().to_str().unwrap()["run-".len()..]
            .parse()
            .unwrap()
    }

    /// Wait (up to 10 s) until `n` stub runs are fully set up.
    fn wait_ready(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let ready = self
                .runs()
                .iter()
                .filter(|r| r.join("ready").exists())
                .count();
            if ready >= n {
                return;
            }
            assert!(Instant::now() < deadline, "stub never became ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn argv_of(run: &Path) -> Vec<String> {
    let raw = std::fs::read(run.join("argv")).unwrap();
    let mut parts: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|p| String::from_utf8(p.to_vec()).unwrap())
        .collect();
    assert_eq!(parts.pop().as_deref(), Some(""), "argv ends with a NUL");
    parts
}

fn read_trimmed(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap().trim().to_string()
}

fn stderr_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn is_lower_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => matches!(c, '0'..='9' | 'a'..='f'),
        })
}

/// The three model fields of `e`, in record order.
fn models(e: &Value) -> [Value; 3] {
    [
        e["requestedModel"].clone(),
        e["effectiveModel"].clone(),
        e["effectivePolicyDigest"].clone(),
    ]
}

fn no_models() -> [Value; 3] {
    [Value::Null, Value::Null, Value::Null]
}

fn all_models() -> [Value; 3] {
    [json!(REQUESTED), json!(EFFECTIVE), json!(DIGEST)]
}

// ---------------------------------------------------------------- clause 1

#[test]
fn argv_is_the_contract_shape_and_carries_no_prompt() {
    let w = World::new();
    let _g = w.guard();
    w.task("argv", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);

    let out = w.run("argv");
    assert!(out.status.success(), "{}", stderr_of(&out));

    let names = w.exports_named();
    assert_eq!(names.len(), 1, "{names:?}");
    let (name, e) = &names[0];
    let attempt = e["attemptId"].as_str().unwrap();
    let session = e["sessionId"].as_str().unwrap();
    assert_eq!(name, &format!("{attempt}.1.json"));
    assert!(is_lower_uuid(session), "{session}");

    let runs = w.runs();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    let repo = w.repo.path();
    assert_eq!(
        argv_of(run),
        vec![
            "--import".to_string(),
            "tsx".into(),
            repo.join("src/launcher/main.ts").display().to_string(),
            "run".into(),
            "--config".into(),
            w.plan_path("argv").display().to_string(),
            "--attempt-ref".into(),
            attempt.into(),
            "--session-id".into(),
            session.into(),
        ]
    );
    assert_eq!(
        PathBuf::from(read_trimmed(&run.join("pwd"))),
        repo.canonicalize().unwrap()
    );
    assert_eq!(read_trimmed(&run.join("stdin")), "/dev/null");
    let stub = World::stub_pid(run);
    assert_eq!(read_trimmed(&run.join("pgid")), stub.to_string());
}

#[test]
fn normal_task_unchanged_with_collector_section() {
    let w = World::new();
    let _g = w.guard();
    let repo = w.repo.path().display().to_string();
    w.cc()
        .args([
            "task",
            "add",
            "normal",
            "--account",
            "work",
            "--at",
            "07:00",
        ])
        .args(["--prompt", "say hi", "--cwd", &repo])
        .assert()
        .success();
    // The schedule went to the fake crontab's table, nowhere else.
    let tab = std::fs::read_to_string(w.data.path().join(".fakebin").join("tab")).unwrap();
    assert!(tab.contains("task run normal"), "{tab}");

    let out = w.run("normal");
    assert!(out.status.success(), "{}", stderr_of(&out));

    // The fake claude ran (so no real one could have), with a prompt run's argv.
    let log = std::fs::read_to_string(w.claude_log()).expect("the fake claude ran");
    let argv: Vec<&str> = log.lines().collect();
    assert_eq!(argv[..2], ["-p", "say hi"], "{argv:?}");
    let v: Value = serde_json::from_slice(&std::fs::read(w.tasks_path()).unwrap()).unwrap();
    let last = v["tasks"]["normal"]["last_session_id"].as_str().unwrap();
    assert!(!last.is_empty());
    let at = argv.iter().position(|a| *a == "--session-id").unwrap();
    assert_eq!(argv[at + 1], last);
    assert_eq!(v["tasks"]["normal"]["last_status"], "ok");

    assert!(w.exports().is_empty(), "a plain task writes no export");
    assert!(
        w.runs().is_empty(),
        "a plain task never starts the launcher"
    );
}

// ---------------------------------------------------------------- clause 2

#[test]
fn two_runs_get_distinct_attempt_ids() {
    let w = World::new();
    let _g = w.guard();
    w.task("twice", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);

    assert!(w.run("twice").status.success());
    assert!(w.run("twice").status.success());

    let ids: Vec<String> = w
        .exports()
        .iter()
        .map(|e| e["attemptId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let argvs: Vec<Vec<String>> = w.runs().iter().map(|r| argv_of(r)).collect();
    assert_eq!(argvs.len(), 2);
    for id in &ids {
        let n = argvs.iter().filter(|a| a.contains(id)).count();
        assert_eq!(n, 1, "{id} is in {n} launcher argvs");
    }
}

// ---------------------------------------------------------------- clause 3

#[test]
fn export_dir_0700_files_0600_no_dotfiles() {
    let w = World::new();
    let _g = w.guard();
    w.task("ok", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);

    assert!(w.run("ok").status.success());
    assert!(!w.run("no-such-task").status.success());

    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&w.export), 0o700);
    let all: Vec<String> = std::fs::read_dir(&w.export)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(all.len(), 2, "{all:?}");
    for name in &all {
        assert!(!name.starts_with('.'), "left behind: {name}");
        assert_eq!(mode(&w.export.join(name)), 0o600, "{name}");
    }
    for (name, e) in w.exports_named() {
        assert_eq!(name, format!("{}.1.json", e["attemptId"].as_str().unwrap()));
    }
}

#[test]
fn unknown_task_with_collector_section_records_launch_failed() {
    let w = World::new();
    let _g = w.guard();

    let out = w.run("no-such-task");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("reason=unknown_task"),
        "{}",
        stderr_of(&out)
    );

    let e = w.only_export();
    assert_eq!(e["taskId"], "no-such-task");
    assert_eq!(e["runnerInstanceId"], "runner-test");
    assert_eq!(e["status"], "launch_failed");
    for k in [
        "safeErrorCode",
        "sessionId",
        "exitCode",
        "startedAt",
        "endedAt",
    ] {
        assert_eq!(e[k], Value::Null, "{k}");
    }
    assert!(w.runs().is_empty());
}

#[test]
fn spawn_enoent_when_node_is_missing() {
    let w = World::new();
    let _g = w.guard();
    w.set_node(&w.bin.path().join("no-node-here"));
    w.task("t", &plan(&[]), WALL, GRACE);

    let out = w.run("t");
    assert_eq!(out.status.code(), Some(1));
    let e = w.only_export();
    assert_eq!(e["status"], "launch_failed");
    assert_eq!(e["safeErrorCode"], "spawn_enoent");
    assert_eq!(e["exitCode"], Value::Null);
    assert!(is_lower_uuid(e["sessionId"].as_str().unwrap()));
    assert!(w.runs().is_empty());
}

#[test]
fn spawn_failed_when_node_is_not_executable() {
    let w = World::new();
    let _g = w.guard();
    let node = w.bin.path().join("node-0644");
    std::fs::write(&node, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o644)).unwrap();
    w.set_node(&node);
    w.task("t", &plan(&[]), WALL, GRACE);

    let out = w.run("t");
    assert_eq!(out.status.code(), Some(1));
    let e = w.only_export();
    assert_eq!(e["status"], "launch_failed");
    assert_eq!(e["safeErrorCode"], "spawn_failed");
    assert_eq!(e["exitCode"], Value::Null);
    assert!(is_lower_uuid(e["sessionId"].as_str().unwrap()));
}

// ---------------------------------------------------------------- clause 4

#[test]
fn launcher_env_has_skip_and_no_force() {
    let w = World::new();
    let _g = w.guard();
    w.task("env", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);

    let out = w.run_env("env", &[("CORTEX_FORCE_RECORD", "1")]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let runs = w.runs();
    assert_eq!(runs.len(), 1);
    let raw = std::fs::read(runs[0].join("environ")).unwrap();
    let vars: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    assert!(vars.iter().any(|v| v == "CORTEX_SKIP_RECORD=1"));
    assert!(
        !vars.iter().any(|v| v.starts_with("CORTEX_FORCE_RECORD=")),
        "the forcing flag reached the launcher"
    );
}

#[test]
fn two_collector_tasks_with_different_wall_timeouts() {
    let w = World::new();
    let _g = w.guard();
    let grace = 2500;
    for (id, wall) in [("wall-1", 1), ("wall-2", 2)] {
        w.task(id, &plan(&[("SLEEP_SECS", "60")]), wall, grace);
    }

    for (id, wall) in [("wall-1", 1u64), ("wall-2", 2)] {
        let t0 = Instant::now();
        let out = w.run(id);
        let took = t0.elapsed();
        assert_eq!(out.status.code(), Some(1));
        let e = w.export_of(id);
        assert_eq!(e["status"], "timed_out", "{id}");
        assert_eq!(e["safeErrorCode"], Value::Null, "{id}");
        let lo = Duration::from_secs(wall);
        let hi = lo + Duration::from_millis(grace) + Duration::from_secs(6);
        assert!(took >= lo && took <= hi, "{id} took {took:?}");
    }
    assert!(gone(&w.pids()));
}

// ---------------------------------------------------------------- clause 5

#[test]
fn busy_records_skipped_busy_within_one_second() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "a",
        &plan(&[("SLEEP_SECS", "5"), ("OUTCOME", &outcome_line())]),
        WALL,
        GRACE,
    );
    w.task("b", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);

    let mut a = ChildGuard(w.spawn("a"));
    w.wait_ready(1);

    let t0 = Instant::now();
    let out = w.run("b");
    let took = t0.elapsed();
    assert!(took < Duration::from_secs(1), "busy took {took:?}");
    assert_eq!(out.status.code(), Some(1));

    let status = a.wait_for(Duration::from_secs(30));
    assert!(status.success(), "{status:?}");

    assert_eq!(w.exports().len(), 2);
    let b = w.export_of("b");
    assert_eq!(b["status"], "skipped_busy");
    assert_eq!(b["safeErrorCode"], "lock_busy");
    for k in ["sessionId", "exitCode", "startedAt", "endedAt"] {
        assert_eq!(b[k], Value::Null, "{k}");
    }
    let a = w.export_of("a");
    assert_eq!(a["status"], "exited_ok");
    assert_eq!(a["safeErrorCode"], Value::Null);
    assert_eq!(models(&a), all_models());
    assert_eq!(w.runs().len(), 1, "the busy attempt started no launcher");
    assert!(gone(&w.pids()));
}

#[test]
fn lock_is_free_after_cc_loadout_is_sigkilled() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "a",
        &plan(&[("SLEEP_SECS", "30"), ("GRANDCHILD", "none")]),
        WALL,
        GRACE,
    );
    w.task(
        "b",
        &plan(&[("SLEEP_SECS", "0"), ("OUTCOME", &outcome_line())]),
        WALL,
        GRACE,
    );

    let mut a = ChildGuard(w.spawn("a"));
    w.wait_ready(1);
    signal_child(&a, "KILL");
    a.wait_for(Duration::from_secs(10));
    // The launcher outlives the runner that held the lock.
    let stub = World::stub_pid(&w.runs()[0]);
    let stub_ref = w.pids().into_iter().find(|p| p.0 == stub).unwrap();
    assert!(!gone(&[stub_ref]), "the stub should still be running");

    let out = w.run("b");
    assert!(out.status.success(), "{}", stderr_of(&out));
    let b = w.export_of("b");
    assert_eq!(b["status"], "exited_ok");
    assert_eq!(b["safeErrorCode"], Value::Null);
    // `a`'s runner died before it could write anything.
    assert_eq!(w.exports().len(), 1);
}

// ---------------------------------------------------------------- clause 6

#[test]
fn wall_timeout_clears_tree_with_term_ignoring_setsid_grandchild() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "t",
        &plan(&[
            ("SLEEP_SECS", "60"),
            ("IGNORE_TERM", "1"),
            ("GRANDCHILD", "setsid"),
            ("GRANDCHILD_IGNORE_TERM", "1"),
        ]),
        1,
        2500,
    );

    let out = w.run("t");
    assert_eq!(out.status.code(), Some(1));
    let e = w.only_export();
    assert_eq!(e["status"], "timed_out");
    assert_eq!(e["safeErrorCode"], Value::Null);
    let pids = w.pids();
    // Stub, its sleep, the grandchild and its sleep.
    assert!(pids.len() >= 4, "{pids:?}");
    assert!(gone(&pids), "left behind: {pids:?}");
}

#[test]
fn sigterm_to_cc_loadout_records_killed_and_clears_tree() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "t",
        &plan(&[("SLEEP_SECS", "30"), ("GRANDCHILD", "plain")]),
        WALL,
        GRACE,
    );

    let mut c = ChildGuard(w.spawn("t"));
    w.wait_ready(1);
    signal_child(&c, "TERM");
    let status = c.wait_for(Duration::from_secs(20));
    assert_eq!(status.code(), Some(1));

    let e = w.only_export();
    assert_eq!(e["status"], "killed");
    assert_eq!(e["safeErrorCode"], Value::Null);
    let pids = w.pids();
    assert!(pids.len() >= 4, "{pids:?}");
    assert!(gone(&pids), "left behind: {pids:?}");
}

#[test]
fn natural_exit_clears_escaped_daemon() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "t",
        &plan(&[
            ("GRANDCHILD", "escape"),
            ("GRANDCHILD_IGNORE_TERM", "1"),
            ("GRANDCHILD_SLEEP_SECS", "60"),
            ("OUTCOME", &outcome_line()),
        ]),
        WALL,
        GRACE,
    );

    let out = w.run("t");
    assert!(out.status.success(), "{}", stderr_of(&out));
    let e = w.only_export();
    assert_eq!(e["status"], "exited_ok");
    assert_eq!(e["safeErrorCode"], Value::Null);
    let run = &w.runs()[0];
    assert!(run.join("gc-ready").exists(), "the daemon never started");
    let pids = w.pids();
    assert!(pids.len() >= 3, "{pids:?}");
    assert!(gone(&pids), "left behind: {pids:?}");
}

#[test]
fn natural_exit_lets_grandchild_finish_within_grace() {
    let w = World::new();
    let _g = w.guard();
    let done = w.data.path().join("grandchild-done");
    w.task(
        "t",
        &plan(&[
            ("GRANDCHILD", "plain"),
            ("GRANDCHILD_SLEEP_SECS", "1"),
            ("GRANDCHILD_DONE_FILE", &done.display().to_string()),
            ("OUTCOME", &outcome_line()),
        ]),
        WALL,
        4000,
    );

    let out = w.run("t");
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        done.exists(),
        "the grandchild was killed before it finished"
    );
    let e = w.only_export();
    assert_eq!(e["status"], "exited_ok");
    assert_eq!(e["safeErrorCode"], Value::Null);
    assert!(gone(&w.pids()));
}

#[test]
fn short_grace_kills_before_launcher_finishes() {
    let w = World::new();
    let _g = w.guard();
    let grace = 2500;
    w.task(
        "t",
        &plan(&[
            ("SLEEP_SECS", "60"),
            ("ON_TERM_SLEEP_SECS", "10"),
            ("ON_TERM_EXIT", "30"),
        ]),
        1,
        grace,
    );

    let t0 = Instant::now();
    let out = w.run("t");
    let took = t0.elapsed();
    assert_eq!(out.status.code(), Some(1));
    // It gave the launcher its whole grace, and not one launcher wind-down more.
    let lo = Duration::from_secs(1) + Duration::from_millis(grace);
    assert!(took >= lo, "took {took:?}");
    assert!(took < lo + Duration::from_secs(6), "took {took:?}");
    let e = w.only_export();
    assert_eq!(e["status"], "timed_out");
    assert_eq!(e["safeErrorCode"], Value::Null);
    // SIGKILLed, so the launcher never got to exit 30.
    assert_eq!(e["exitCode"], Value::Null);
    let pids = w.pids();
    assert!(gone(&pids), "left behind: {pids:?}");
}

// ---------------------------------------------------------------- clause 7

#[test]
fn collector_run_does_not_touch_tasks_json() {
    let w = World::new();
    let _g = w.guard();
    w.task("ok", &plan(&[("OUTCOME", &outcome_line())]), WALL, GRACE);
    w.task("err", &plan(&[("EXIT_CODE", "1")]), WALL, GRACE);

    for id in ["ok", "err", "no-such-task"] {
        let before = std::fs::read(w.tasks_path()).unwrap();
        let _ = w.run(id);
        let after = std::fs::read(w.tasks_path()).unwrap();
        assert!(before == after, "task run {id} changed tasks.json");
    }
    assert_eq!(w.exports().len(), 3);
}

// ---------------------------------------------------------------- clause 8

#[test]
fn stdout_and_stderr_markers_never_reach_cc_loadout_output() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "t",
        &plan(&[
            ("OUTCOME", "MARKER-OUT"),
            ("STDERR_LINES", "MARKER-ERR|geralt-launcher ok k=v"),
        ]),
        WALL,
        GRACE,
    );

    let out = w.run("t");
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr_of(&out);
    assert!(err.contains("geralt-launcher ok k=v"), "{err}");
    assert!(
        err.contains("cc-loadout-collector attempt_recorded"),
        "{err}"
    );
    assert!(
        !err.contains("MARKER-OUT") && !err.contains("MARKER-ERR"),
        "{err}"
    );
    let e = w.only_export();
    assert_eq!(e["status"], "exited_ok");
    assert_eq!(models(&e), no_models());
}

#[test]
fn oversized_stdout_gives_null_models_and_does_not_hang() {
    let w = World::new();
    let _g = w.guard();
    w.task(
        "t",
        &plan(&[("STDOUT_BYTES", "1048576"), ("OUTCOME", &outcome_line())]),
        WALL,
        GRACE,
    );

    let t0 = Instant::now();
    let out = w.run("t");
    assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", t0.elapsed());
    assert!(out.status.success(), "{}", stderr_of(&out));
    let e = w.only_export();
    assert_eq!(e["status"], "exited_ok");
    assert_eq!(models(&e), no_models());
}

#[test]
fn pipe_held_by_grandchild_does_not_hang() {
    let w = World::new();
    let _g = w.guard();
    let grace = 2500;
    w.task(
        "t",
        &plan(&[
            ("GRANDCHILD", "setsid"),
            ("GRANDCHILD_HOLDS_STDOUT", "1"),
            ("GRANDCHILD_SLEEP_SECS", "60"),
            ("OUTCOME", &outcome_line()),
        ]),
        WALL,
        grace,
    );

    let t0 = Instant::now();
    let out = w.run("t");
    let took = t0.elapsed();
    assert!(
        took < Duration::from_millis(grace) + Duration::from_secs(6),
        "took {took:?}"
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let e = w.only_export();
    assert_eq!(e["status"], "exited_ok");
    assert_eq!(e["safeErrorCode"], Value::Null);
    let pids = w.pids();
    assert!(pids.len() >= 3, "{pids:?}");
    assert!(gone(&pids), "left behind: {pids:?}");
}

// ---------------------------------------------------------------- clause 9

/// What the record of one launcher exit must say: status, safeErrorCode, exitCode.
type ExitRow = (&'static str, Option<&'static str>, Option<i32>);

#[test]
fn exit_table_end_to_end() {
    let w = World::new();
    let _g = w.guard();
    let full = outcome_line();
    let all = all_models();
    let none = no_models();
    let requested_only = [json!(REQUESTED), Value::Null, Value::Null];
    let failed = |code| ("launch_failed", Some(code), None);
    // launcher exit -> (status, safeErrorCode, record exitCode), record models
    let rows: Vec<(i32, ExitRow, &[Value; 3])> = vec![
        (0, ("exited_ok", None, Some(0)), &all),
        (1, ("exited_error", Some("launcher_error"), Some(1)), &all),
        (2, failed("config_invalid"), &requested_only),
        (3, failed("assemble_failed"), &requested_only),
        (4, failed("spawn_enoent"), &requested_only),
        (5, failed("spawn_failed"), &requested_only),
        (6, failed("cli_version_mismatch"), &requested_only),
        (7, failed("auth_required"), &requested_only),
        (8, failed("preflight_failed"), &requested_only),
        (9, failed("control_protocol_error"), &requested_only),
        (
            20,
            ("exited_error", Some("runtime_mismatch"), Some(20)),
            &all,
        ),
        (21, ("exited_error", Some("turn_limit"), Some(21)), &all),
        (22, ("exited_error", Some("session_error"), Some(22)), &all),
        (30, ("exited_error", None, Some(30)), &none),
        (99, ("exited_error", None, Some(99)), &none),
    ];

    for (code, (status, safe, exit), want_models) in rows {
        let id = format!("exit-{code}");
        let code_s = code.to_string();
        // 0-22 print an outcome line with all three values (for 2-9 the runner
        // must drop the last two); 30 and 99 print none.
        let mut kv = vec![("EXIT_CODE", code_s.as_str())];
        if code < 30 {
            kv.push(("OUTCOME", full.as_str()));
        }
        w.task(&id, &plan(&kv), WALL, GRACE);

        let out = w.run(&id);
        let e = w.export_of(&id);
        assert_eq!(e["status"], status, "exit {code}");
        assert_eq!(e["safeErrorCode"], json!(safe), "exit {code}");
        assert_eq!(e["exitCode"], json!(exit), "exit {code}");
        assert_eq!(&models(&e), want_models, "exit {code}");
        assert!(
            is_lower_uuid(e["sessionId"].as_str().unwrap()),
            "exit {code}"
        );
        assert!(
            e["startedAt"].is_string() && e["endedAt"].is_string(),
            "exit {code}"
        );
        let want_exit = if code == 0 { 0 } else { 1 };
        assert_eq!(out.status.code(), Some(want_exit), "exit {code}");
    }
    assert!(gone(&w.pids()));
}
