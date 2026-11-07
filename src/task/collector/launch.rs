//! Starting Geralt's collector launcher and handling its two output pipes. The
//! launcher's stdout is at most one outcome line, kept for the parser and
//! otherwise discarded; its stderr is made of fixed-code lines that may be
//! forwarded to this process's stderr, nothing else from it ever is. Neither
//! pipe can stall the launcher by filling up, and neither can stall the runner by
//! staying open.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::task::collector::outcome::STDOUT_CAP_BYTES;
use crate::task::config::{CollectorSettings, CollectorTask};

/// How long the runner waits, once the launcher is gone, for its two pipes to
/// reach end-of-file. A grandchild that inherited a pipe and keeps running holds
/// it open for as long as it likes; the runner does not.
pub const CLOSE_WAIT_MS: u64 = 500;
/// A launcher stderr line longer than this (the newline not counted) is dropped
/// whole, whatever it says.
const STDERR_LINE_CAP_BYTES: usize = 4096;
/// Most launcher stderr, newlines included, that is forwarded in one attempt.
const STDERR_FORWARD_CAP_BYTES: usize = 64 * 1024;
/// Only lines that start like this are forwarded: the launcher's fixed-code log.
const FORWARD_PREFIX: &[u8] = b"geralt-launcher ";

/// The command that runs the launcher for one attempt:
/// `node --import tsx <repo>/src/launcher/main.ts run --config <launcher_config>
/// --attempt-ref <attempt> --session-id <session>`, in the Geralt checkout, in
/// its own process group, with no stdin and both output pipes captured. The
/// environment is the runner's own, except that the Cortex hook is told not to
/// record the session and never forced to; `CLAUDE_CONFIG_DIR` is set only for an
/// isolated account.
pub fn launcher_command(
    s: &CollectorSettings,
    t: &CollectorTask,
    attempt: &str,
    session: &str,
    cfg_dir: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(&s.node);
    cmd.args(["--import", "tsx"])
        .arg(s.geralt_repo.join("src").join("launcher").join("main.ts"))
        .args(["run", "--config"])
        .arg(&t.launcher_config)
        .args(["--attempt-ref", attempt, "--session-id", session])
        .current_dir(&s.geralt_repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so the runner can signal the launcher and what it
        // started without touching itself or the shell that ran cron.
        .process_group(0)
        // The attempt is a collector run, not a work session worth recording;
        // and the forcing flag a plain scheduled task sets must never leak in.
        .env("CORTEX_SKIP_RECORD", "1")
        .env_remove("CORTEX_FORCE_RECORD");
    if let Some(dir) = cfg_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    cmd
}

/// What was kept of the launcher's output.
///
/// `Debug` is written by hand: `stdout` is the launcher's raw output, which is
/// never printed, so a failed assertion or a stray `{:?}` shows sizes and flags
/// only.
#[derive(Clone, PartialEq, Eq)]
pub struct Captured {
    /// The first [`STDOUT_CAP_BYTES`] of stdout.
    pub stdout: Vec<u8>,
    /// Stdout had more than that; the rest was read and discarded.
    pub stdout_truncated: bool,
    /// Stderr lines that were counted but not forwarded.
    pub stderr_dropped: usize,
}

impl std::fmt::Debug for Captured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Captured")
            .field("stdout_len", &self.stdout.len())
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_dropped", &self.stderr_dropped)
            .finish()
    }
}

/// Both pipes of a started launcher, each drained by its own detached thread.
pub struct OutputPumps {
    shared: Arc<Shared>,
}

impl OutputPumps {
    /// Take the child's stdout and stderr pipes and start draining them.
    /// Forwarded stderr lines go to this process's stderr; `attempt` only names
    /// the threads.
    pub fn start(child: &mut Child, attempt: &str) -> Self {
        Self::start_with_sink(child, attempt, StderrSink)
    }

    fn start_with_sink<W: Write + Send + 'static>(
        child: &mut Child,
        attempt: &str,
        mut sink: W,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        if let Some(out) = child.stdout.take() {
            spawn_pump(&shared, format!("collector-stdout-{attempt}"), move |sh| {
                pump_stdout(out, sh)
            });
        }
        if let Some(err) = child.stderr.take() {
            spawn_pump(&shared, format!("collector-stderr-{attempt}"), move |sh| {
                pump_stderr(err, &mut sink, sh)
            });
        }
        OutputPumps { shared }
    }

    /// Wait up to `wait` for both pipes to reach end-of-file, then return what
    /// has been kept so far. A pipe that is still open (a grandchild holds it) is
    /// not waited for and not joined; its thread ends when the pipe does.
    pub fn finish(self, wait: Duration) -> Captured {
        let guard = self.shared.lock();
        let (mut st, _) = self
            .shared
            .changed
            .wait_timeout_while(guard, wait, |s| s.open > 0)
            .unwrap_or_else(PoisonError::into_inner);
        Captured {
            stdout: std::mem::take(&mut st.stdout),
            stdout_truncated: st.stdout_truncated,
            stderr_dropped: st.stderr_dropped,
        }
    }
}

/// Forwarded stderr lines go here: this process's stderr, locked once per line so
/// a line is never split by another writer in this process.
struct StderrSink;

impl Write for StderrSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stderr().lock().write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::stderr().lock().write_all(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().lock().flush()
    }
}

/// State the two pump threads and `finish` share.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    stdout: Vec<u8>,
    stdout_truncated: bool,
    stderr_dropped: usize,
    /// Pump threads that have not yet seen end-of-file.
    open: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One pump thread has seen end-of-file (or given up on its pipe).
    fn close_one(&self) {
        let mut st = self.lock();
        st.open = st.open.saturating_sub(1);
        self.changed.notify_all();
    }
}

/// Counts a pump thread as closed when it ends, however it ends.
struct CloseOnDrop(Arc<Shared>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close_one();
    }
}

/// Run `body` on a new detached thread named `name`, counted as open until it
/// ends. If the thread cannot be made, `body` (and the pipe it owns) is dropped
/// right away, which closes the pipe: the launcher gets a broken pipe rather
/// than a full one.
fn spawn_pump(shared: &Arc<Shared>, name: String, body: impl FnOnce(&Shared) + Send + 'static) {
    shared.lock().open += 1;
    let closer = CloseOnDrop(Arc::clone(shared));
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        body(&closer.0);
        // `closer` drops here, and also if `body` panics.
    });
    // A failed spawn drops the closure and with it `closer`, which un-counts it.
    drop(spawned);
}

/// Keep the first [`STDOUT_CAP_BYTES`] of `r`, discard the rest, read to the end.
fn pump_stdout<R: Read>(mut r: R, shared: &Shared) {
    let mut chunk = [0u8; 8192];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let mut st = shared.lock();
                let keep = n.min(STDOUT_CAP_BYTES - st.stdout.len());
                st.stdout.extend_from_slice(&chunk[..keep]);
                if keep < n {
                    st.stdout_truncated = true;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Read `r` line by line. A line that starts with [`FORWARD_PREFIX`], is at most
/// [`STDERR_LINE_CAP_BYTES`] long (newline not counted) and still fits in the
/// [`STDERR_FORWARD_CAP_BYTES`] budget is written to `out` as it came, plus its
/// newline; every other line is only counted. A line is never held in memory
/// beyond its cap. Write errors are ignored: forwarding must not end an attempt.
fn pump_stderr<R: Read, W: Write>(r: R, out: &mut W, shared: &Shared) {
    let mut reader = BufReader::with_capacity(8192, r);
    let mut line: Vec<u8> = Vec::new();
    let mut overlong = false;
    let mut forwarded = 0usize;

    let mut finish_line = |line: &mut Vec<u8>, overlong: &mut bool| {
        let budget_left = STDERR_FORWARD_CAP_BYTES.saturating_sub(forwarded);
        let forward = !*overlong && line.starts_with(FORWARD_PREFIX) && line.len() < budget_left;
        if forward {
            line.push(b'\n');
            let _ = out.write_all(line);
            forwarded += line.len();
        } else {
            shared.lock().stderr_dropped += 1;
        }
        line.clear();
        *overlong = false;
    };

    loop {
        let buf = match reader.fill_buf() {
            Ok([]) => break,
            Ok(buf) => buf,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let (piece, ends_line) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (&buf[..i], true),
            None => (buf, false),
        };
        if !overlong {
            if line.len() + piece.len() > STDERR_LINE_CAP_BYTES {
                overlong = true;
                line.clear();
            } else {
                line.extend_from_slice(piece);
            }
        }
        let used = piece.len() + usize::from(ends_line);
        reader.consume(used);
        if ends_line {
            finish_line(&mut line, &mut overlong);
        }
    }
    // The last line may end without a newline.
    if overlong || !line.is_empty() {
        finish_line(&mut line, &mut overlong);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::task::collector::test_support::KillGrandchild;

    fn settings() -> CollectorSettings {
        CollectorSettings {
            node: PathBuf::from("/opt/node/bin/node"),
            geralt_repo: PathBuf::from("/opt/geralt"),
            export_dir: PathBuf::from("/var/geralt/export"),
            runner_instance_id: "runner-test".into(),
        }
    }

    fn task() -> CollectorTask {
        CollectorTask {
            launcher_config: PathBuf::from("/etc/geralt/launcher.json"),
            wall_timeout_secs: 600,
            grace_ms: 5000,
        }
    }

    fn envs(cmd: &Command) -> BTreeMap<OsString, Option<OsString>> {
        cmd.get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(OsString::from)))
            .collect()
    }

    #[test]
    fn launcher_command_has_the_contract_shape() {
        // Only built and inspected, never spawned: the program is Geralt's real
        // launcher.
        let cmd = launcher_command(&settings(), &task(), "att-1", "sess-1", None);
        assert_eq!(cmd.get_program(), "/opt/node/bin/node");
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "--import",
                "tsx",
                "/opt/geralt/src/launcher/main.ts",
                "run",
                "--config",
                "/etc/geralt/launcher.json",
                "--attempt-ref",
                "att-1",
                "--session-id",
                "sess-1",
            ]
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/opt/geralt")));

        // The live account: the Cortex flags and nothing else are touched.
        let env = envs(&cmd);
        assert_eq!(
            env,
            BTreeMap::from([
                (
                    OsString::from("CORTEX_SKIP_RECORD"),
                    Some(OsString::from("1"))
                ),
                (OsString::from("CORTEX_FORCE_RECORD"), None),
            ])
        );

        // An isolated account also gets its config dir.
        let dir = Path::new("/data/accounts/work/run/cfg");
        let cmd = launcher_command(&settings(), &task(), "att-1", "sess-1", Some(dir));
        let env = envs(&cmd);
        assert_eq!(env.len(), 3);
        assert_eq!(
            env.get(&OsString::from("CLAUDE_CONFIG_DIR")),
            Some(&Some(dir.as_os_str().to_os_string()))
        );
        assert_eq!(
            env.get(&OsString::from("CORTEX_SKIP_RECORD")),
            Some(&Some(OsString::from("1")))
        );
        assert_eq!(env.get(&OsString::from("CORTEX_FORCE_RECORD")), Some(&None));
    }

    /// Kills and reaps a test's child when dropped, so a failed assertion leaves
    /// no process behind.
    struct Reaped(Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// `sh -c script` with both output pipes captured and no stdin.
    fn sh(script: &str) -> Reaped {
        let child = crate::util::retry_etxtbsy(|| {
            Command::new("sh")
                .arg("-c")
                .arg(script)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .unwrap();
        Reaped(child)
    }

    fn wait(child: &mut Reaped) -> std::process::ExitStatus {
        crate::account::prime::wait_with_timeout(&mut child.0, Duration::from_secs(20)).unwrap()
    }

    /// A `Write` that keeps what it is given where the test can read it.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn pumps_truncate_stdout_at_4096_and_keep_draining() {
        // A megabyte on stdout is far more than a pipe holds: the child can only
        // finish if the pump keeps reading after it has stopped keeping.
        let mut child = sh("head -c 1048576 /dev/zero");
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        let status = wait(&mut child);
        let got = pumps.finish(LONG);
        assert!(status.success());
        assert_eq!(got.stdout.len(), 4096);
        assert!(got.stdout.iter().all(|b| *b == 0));
        assert!(got.stdout_truncated);
        assert_eq!(got.stderr_dropped, 0);
    }

    #[test]
    fn stdout_that_fits_is_kept_whole_and_not_marked_truncated() {
        let mut child = sh("printf 'one line\\n'");
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        wait(&mut child);
        let got = pumps.finish(LONG);
        assert_eq!(got.stdout, b"one line\n");
        assert!(!got.stdout_truncated);

        // Exactly the cap is not over it.
        let mut child = sh("head -c 4096 /dev/zero");
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        wait(&mut child);
        let got = pumps.finish(LONG);
        assert_eq!(got.stdout.len(), 4096);
        assert!(!got.stdout_truncated);

        // One byte more is.
        let mut child = sh("head -c 4097 /dev/zero");
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        wait(&mut child);
        let got = pumps.finish(LONG);
        assert_eq!(got.stdout.len(), 4096);
        assert!(got.stdout_truncated);
    }

    #[test]
    fn pumps_forward_only_geralt_launcher_lines() {
        // One forwarded line, one foreign line, one 5000-byte launcher line. The
        // forwarded text goes to a sink here; what reaches the real stderr is
        // checked by the binary tests.
        let mut child = sh("echo 'geralt-launcher x k=v' >&2; \
             echo 'MARKER-STDERR' >&2; \
             { printf 'geralt-launcher '; head -c 4984 /dev/zero | tr '\\0' 'a'; echo; } >&2");
        let sink = Sink::default();
        let pumps = OutputPumps::start_with_sink(&mut child.0, "att-test", sink.clone());
        wait(&mut child);
        let got = pumps.finish(LONG);
        assert_eq!(got.stderr_dropped, 2);
        assert_eq!(sink.text(), "geralt-launcher x k=v\n");
        assert!(got.stdout.is_empty());
    }

    /// Run `lines` through the stderr pump and return (forwarded text, dropped).
    fn pump(lines: &[u8]) -> (Vec<u8>, usize) {
        let shared = Shared::default();
        let mut out = Vec::new();
        pump_stderr(lines, &mut out, &shared);
        let dropped = shared.lock().stderr_dropped;
        (out, dropped)
    }

    #[test]
    fn a_stderr_line_is_forwarded_whole_with_its_newline() {
        let (out, dropped) = pump(b"geralt-launcher a k=v\ngeralt-launcher b\n");
        assert_eq!(out, b"geralt-launcher a k=v\ngeralt-launcher b\n");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn the_prefix_needs_its_space() {
        let (out, dropped) = pump(b"geralt-launcher\ngeralt-launcherx y\nxgeralt-launcher y\n ");
        assert!(out.is_empty());
        // Three lines, and the lone space after the last newline is a fourth.
        assert_eq!(dropped, 4);
    }

    #[test]
    fn a_last_line_without_a_newline_is_still_a_line() {
        let (out, dropped) = pump(b"geralt-launcher a\ngeralt-launcher tail");
        assert_eq!(out, b"geralt-launcher a\ngeralt-launcher tail\n");
        assert_eq!(dropped, 0);
        let (out, dropped) = pump(b"MARKER with no newline");
        assert!(out.is_empty());
        assert_eq!(dropped, 1);
        let (out, dropped) = pump(b"");
        assert!(out.is_empty());
        assert_eq!(dropped, 0);
    }

    #[test]
    fn the_line_cap_is_4096_bytes_without_the_newline() {
        let fits = format!(
            "geralt-launcher {}\n",
            "a".repeat(STDERR_LINE_CAP_BYTES - FORWARD_PREFIX.len())
        );
        let over = format!(
            "geralt-launcher {}\n",
            "a".repeat(STDERR_LINE_CAP_BYTES - FORWARD_PREFIX.len() + 1)
        );
        let (out, dropped) = pump(format!("{fits}{over}geralt-launcher after\n").as_bytes());
        assert_eq!(out, format!("{fits}geralt-launcher after\n").as_bytes());
        assert_eq!(dropped, 1);

        // A very long line is skipped without being kept, and without taking the
        // next line with it.
        let huge = format!("geralt-launcher {}\n", "b".repeat(1 << 20));
        let (out, dropped) = pump(format!("{huge}geralt-launcher ok\n").as_bytes());
        assert_eq!(out, b"geralt-launcher ok\n");
        assert_eq!(dropped, 1);
    }

    #[test]
    fn forwarding_stops_at_64_kib_and_the_rest_is_counted() {
        // 100-byte lines (99 + newline), 700 of them.
        let line = format!("geralt-launcher {}\n", "c".repeat(83));
        assert_eq!(line.len(), 100);
        let all = line.repeat(700);
        let (out, dropped) = pump(all.as_bytes());
        assert_eq!(out.len(), 655 * 100);
        assert!(out.len() <= STDERR_FORWARD_CAP_BYTES);
        assert_eq!(dropped, 700 - 655);
        // What was forwarded is a prefix of the input, whole lines only.
        assert_eq!(out, all.as_bytes()[..out.len()]);
    }

    #[test]
    fn finish_does_not_wait_for_a_grandchild_that_holds_the_pipes() {
        // The shell writes its grandchild's pid to a file and exits; the
        // grandchild keeps stdout and stderr open for a while. `finish` must give
        // up on it. The pid goes to a file, not to the captured stdout, so the
        // grandchild is stopped even if the pumps under test are what is broken.
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let _cleanup = KillGrandchild(pid_file.clone());
        let mut child = sh(&format!("sleep 30 &\necho $! > '{}'", pid_file.display()));
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        wait(&mut child);
        let start = std::time::Instant::now();
        let got = pumps.finish(Duration::from_millis(200));
        let waited = start.elapsed();

        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            !pid.trim().is_empty() && pid.trim().bytes().all(|b| b.is_ascii_digit()),
            "the shell wrote no pid"
        );
        assert!(got.stdout.is_empty(), "the shell printed to stdout");
        assert!(
            waited >= Duration::from_millis(150) && waited < Duration::from_secs(10),
            "finish took {waited:?}"
        );
    }

    #[test]
    fn captured_debug_shows_sizes_and_flags_never_the_bytes() {
        let captured = Captured {
            stdout: b"SECRET-STDOUT-MARKER".to_vec(),
            stdout_truncated: true,
            stderr_dropped: 3,
        };
        for shown in [format!("{captured:?}"), format!("{captured:#?}")] {
            assert!(!shown.contains("SECRET"), "{shown}");
            // Neither as text nor as the list of byte values a derived Debug prints.
            assert!(!shown.contains("83, 69, 67"), "{shown}");
            assert!(shown.contains("stdout_len: 20"), "{shown}");
            assert!(shown.contains("stdout_truncated: true"), "{shown}");
            assert!(shown.contains("stderr_dropped: 3"), "{shown}");
        }
    }

    #[test]
    fn finish_returns_as_soon_as_both_pipes_are_closed() {
        let mut child = sh("echo out; echo err >&2");
        let pumps = OutputPumps::start(&mut child.0, "att-test");
        wait(&mut child);
        let start = std::time::Instant::now();
        let got = pumps.finish(Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(got.stdout, b"out\n");
        assert_eq!(got.stderr_dropped, 1);
    }

    /// A stand-in for `node` that reports how it was started on stdout, one
    /// `key=value` per line, and exits 0. It runs nothing else.
    fn fixture_node(dir: &Path) -> PathBuf {
        let p = dir.join("node");
        std::fs::write(
            &p,
            "#!/bin/sh\n\
             echo \"pid=$$\"\n\
             echo \"pgid=$(awk '{print $5}' /proc/$$/stat)\"\n\
             if read -r _line; then echo stdin=data; else echo stdin=eof; fi\n\
             echo \"cwd=$(pwd)\"\n\
             echo \"skip=${CORTEX_SKIP_RECORD-unset}\"\n\
             echo \"cfg=${CLAUDE_CONFIG_DIR-unset}\"\n\
             for a in \"$@\"; do echo \"arg=$a\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_launcher_runs_in_its_own_group_with_no_stdin_and_the_contract_argv() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut s = settings();
        s.node = fixture_node(dir.path());
        s.geralt_repo = repo.clone();

        let cfg = dir.path().join("cfg");
        let child = crate::util::retry_etxtbsy(|| {
            launcher_command(&s, &task(), "att-9", "sess-9", Some(&cfg)).spawn()
        })
        .unwrap();
        let mut child = Reaped(child);
        let pumps = OutputPumps::start(&mut child.0, "att-9");
        let status = wait(&mut child);
        let got = pumps.finish(LONG);
        assert!(status.success());

        let text = String::from_utf8(got.stdout).unwrap();
        let field = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .map(str::to_string)
        };
        let pid = field("pid").unwrap();
        assert_eq!(field("pgid").unwrap(), pid, "own process group: {text}");
        assert_eq!(pid, child.0.id().to_string());
        assert_eq!(field("stdin").unwrap(), "eof");
        assert_eq!(
            std::fs::canonicalize(field("cwd").unwrap()).unwrap(),
            std::fs::canonicalize(&repo).unwrap()
        );
        assert_eq!(field("skip").unwrap(), "1");
        assert_eq!(field("cfg").unwrap(), cfg.to_str().unwrap());
        let args: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("arg="))
            .collect();
        let main_ts = repo.join("src/launcher/main.ts");
        assert_eq!(
            args,
            [
                "--import",
                "tsx",
                main_ts.to_str().unwrap(),
                "run",
                "--config",
                "/etc/geralt/launcher.json",
                "--attempt-ref",
                "att-9",
                "--session-id",
                "sess-9",
            ]
        );
    }
}
