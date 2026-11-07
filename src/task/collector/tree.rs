//! Supervising one collector attempt's process tree: the wall timer, the
//! reaction to a manual kill, and the clean-up that leaves no process of the
//! attempt behind (contract §6; C11–C13).
//!
//! The algorithm in [`supervise`] reaches the operating system only through
//! [`Launcher`] and [`ProcOps`], so it is platform-independent and tested with
//! scripted fakes. [`LinuxProcOps`] is the real side: `/proc` and the signals of
//! [`crate::util::sys`].

use std::io;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::task::collector::outcome::{Observation, Observed};
use crate::task::config::CollectorTask;
use crate::util::Sig;

/// How often the supervision looks at the launcher and its tree (C11).
pub const POLL_MS: u64 = 50;
/// After the launcher ended by itself: how long what it left behind has between
/// a SIGTERM and a SIGKILL (C11).
pub const TERM_SETTLE_MS: u64 = 1000;
/// After the SIGKILLs: how long the supervision waits to see the tree gone
/// before it counts what is left as residual (C11).
pub const VERIFY_MS: u64 = 5000;

/// A process as the supervision knows it: its pid and its start time (clock
/// ticks since boot), which together tell it apart from a later process that
/// was given the same pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcRef {
    pub pid: i32,
    pub start_time: u64,
}

/// The process table, as far as one attempt's supervision may see and touch it.
pub trait ProcOps {
    /// Every live (not zombie) process of the tree being watched.
    fn descendants(&self) -> Vec<ProcRef>;
    /// Send `sig` to `p`, but only if `p.pid` is still the process that started
    /// at `p.start_time`.
    fn signal(&self, p: ProcRef, sig: Sig);
    /// Send `sig` to process group `pgid`.
    fn signal_group(&self, pgid: i32, sig: Sig);
    /// Reap every ended orphan the supervising process has inherited, except
    /// `except_pid` (the launcher, which its own handle reaps).
    fn reap_orphans(&self, except_pid: i32);
}

/// The launcher process, as the supervision waits for it.
pub trait Launcher {
    fn pid(&self) -> i32;
    /// `Ok(Some)` once it has ended and been reaped, `Ok(None)` while it runs.
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
}

impl Launcher for std::process::Child {
    fn pid(&self) -> i32 {
        // A pid that does not fit is not a pid; 0 is one every signal refuses.
        i32::try_from(self.id()).unwrap_or(0)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        std::process::Child::try_wait(self)
    }
}

/// The time limits of one supervision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperviseCfg {
    /// The launcher's whole running time before the runner stops it.
    pub wall: Duration,
    /// Between the SIGTERM to the launcher's group and the SIGKILL; also how long
    /// what a launcher that ended by itself left behind may live on unasked.
    pub grace: Duration,
    pub poll: Duration,
    pub term_settle: Duration,
    pub verify: Duration,
}

impl SuperviseCfg {
    /// The limits of task `t`: its own wall timeout and grace, and the fixed
    /// constants for the rest.
    pub fn for_task(t: &CollectorTask) -> Self {
        SuperviseCfg {
            wall: Duration::from_secs(t.wall_timeout_secs),
            grace: Duration::from_millis(t.grace_ms),
            poll: Duration::from_millis(POLL_MS),
            term_settle: Duration::from_millis(TERM_SETTLE_MS),
            verify: Duration::from_millis(VERIFY_MS),
        }
    }
}

/// How the supervised attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Supervised {
    pub observation: Observation,
    pub observed: Observed,
    /// Processes of the tree still alive when the supervision gave up on them.
    pub residual_count: usize,
}

/// Supervise launcher `l` until it and every process of its tree are gone, or
/// the time to make them go is up.
///
/// While the launcher lives, its own end, the wall timer or the manual-kill flag
/// `term` ends the first phase, checked in that order every `poll`. The timer and
/// the flag count only then (C12): once the launcher has ended by itself its
/// result stands, and the clean-up after it is bounded by
/// `grace + term_settle + verify`.
///
/// - Stopped by the runner: SIGTERM to the launcher's group, which is all that is
///   asked of it, then the full `grace` for it and its tree to go.
/// - Ended by itself: `grace` for what it left behind to go unasked, then a
///   SIGTERM to each process still there and `term_settle` more.
///
/// Then a SIGKILL to the group (only while the launcher is unreaped, so its pid
/// is still its own) and to each process still there, and up to `verify` to see
/// them gone. What is still alive after that is the residual.
pub fn supervise(
    l: &mut dyn Launcher,
    ops: &dyn ProcOps,
    term: &AtomicBool,
    cfg: &SuperviseCfg,
) -> Supervised {
    let pid = l.pid();
    let mut w = LauncherWatch { l, end: None };
    let mut observed = Observed::default();
    let start = Instant::now();

    let ended_by_itself = loop {
        if w.gone() {
            break true;
        }
        if start.elapsed() >= cfg.wall {
            observed.timed_out = true;
            break false;
        }
        if term.load(Ordering::SeqCst) {
            observed.killed = true;
            break false;
        }
        ops.reap_orphans(pid);
        std::thread::sleep(cfg.poll);
    };

    let settled = if ended_by_itself {
        settle(&mut w, ops, pid, cfg.grace, cfg.poll) || {
            for p in ops.descendants() {
                ops.signal(p, Sig::Term);
            }
            settle(&mut w, ops, pid, cfg.term_settle, cfg.poll)
        }
    } else {
        ops.signal_group(pid, Sig::Term);
        settle(&mut w, ops, pid, cfg.grace, cfg.poll)
    };

    if !settled {
        if !w.gone() {
            ops.signal_group(pid, Sig::Kill);
        }
        for p in ops.descendants() {
            ops.signal(p, Sig::Kill);
        }
        settle(&mut w, ops, pid, cfg.verify, cfg.poll);
    }

    let residual_count = ops.descendants().len();
    observed.residual = residual_count > 0;
    let observation = match w.end {
        Some(Some(status)) => status
            .code()
            .map_or(Observation::Signaled, Observation::Exited),
        // Never reaped, or lost to a failed wait: its end was not seen.
        Some(None) | None => Observation::NotReaped,
    };
    Supervised {
        observation,
        observed,
        residual_count,
    }
}

/// The launcher and what is known of its end.
struct LauncherWatch<'a> {
    l: &'a mut dyn Launcher,
    /// `Some(Some(status))` once reaped; `Some(None)` once waiting for it failed
    /// (it is no longer this process's child, so it is gone and its pid is not
    /// its own any more); `None` while it runs.
    end: Option<Option<ExitStatus>>,
}

impl LauncherWatch<'_> {
    /// Look at the launcher once (until its end is known); true once it is gone.
    fn gone(&mut self) -> bool {
        if self.end.is_none() {
            match self.l.try_wait() {
                Ok(Some(status)) => self.end = Some(Some(status)),
                Ok(None) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => self.end = Some(None),
            }
        }
        self.end.is_some()
    }
}

/// Wait up to `within` for the launcher and every process of the tree to be
/// gone, reaping as they end; true if they all went.
fn settle(
    w: &mut LauncherWatch<'_>,
    ops: &dyn ProcOps,
    pid: i32,
    within: Duration,
    poll: Duration,
) -> bool {
    let start = Instant::now();
    loop {
        let launcher_gone = w.gone();
        ops.reap_orphans(pid);
        if launcher_gone && ops.descendants().is_empty() {
            return true;
        }
        let elapsed = start.elapsed();
        if elapsed >= within {
            return false;
        }
        std::thread::sleep(poll.min(within - elapsed));
    }
}

/// The process table of `/proc`, with the signals of [`crate::util::sys`].
#[cfg(target_os = "linux")]
pub struct LinuxProcOps {
    root: Root,
}

#[cfg(target_os = "linux")]
enum Root {
    /// Everything below this process, which is the subreaper of the attempt:
    /// orphans of the launcher's tree come back to it and are reaped here.
    Own(i32),
    /// The launcher and what descends from it, for a caller that shares its
    /// process with others (the unit tests) and must not touch their children:
    /// no orphan is reaped, and once the launcher is reaped (or its pid is
    /// someone else's) nothing is seen, because its orphans went elsewhere.
    #[cfg_attr(not(test), allow(dead_code))]
    Launcher(ProcRef),
}

/// One `/proc/<pid>/stat` entry.
#[cfg(target_os = "linux")]
struct Entry {
    pid: i32,
    state: char,
    ppid: i32,
    start_time: u64,
}

#[cfg(target_os = "linux")]
impl LinuxProcOps {
    /// The whole tree below this process: for the runner, after it has made
    /// itself the child subreaper.
    pub fn own() -> Self {
        let me = i32::try_from(std::process::id()).unwrap_or(0);
        LinuxProcOps {
            root: Root::Own(me),
        }
    }

    /// Only launcher `pid` and its descendants. Call it while `pid` is
    /// unreaped, so the start time read now is the launcher's.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn launcher_subtree(pid: i32) -> Self {
        let start_time = stat_of(pid).map_or(0, |e| e.start_time);
        LinuxProcOps {
            root: Root::Launcher(ProcRef { pid, start_time }),
        }
    }

    fn table() -> Vec<Entry> {
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(stat_of)
            .collect()
    }
}

#[cfg(target_os = "linux")]
fn stat_of(pid: i32) -> Option<Entry> {
    let line = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (state, ppid, start_time) = parse_stat(&line)?;
    Some(Entry {
        pid,
        state,
        ppid,
        start_time,
    })
}

#[cfg(target_os = "linux")]
fn is_dead(state: char) -> bool {
    matches!(state, 'Z' | 'X' | 'x')
}

#[cfg(target_os = "linux")]
impl ProcOps for LinuxProcOps {
    fn descendants(&self) -> Vec<ProcRef> {
        let table = Self::table();
        let mut found = Vec::new();
        let mut frontier = match self.root {
            Root::Own(me) => vec![me],
            Root::Launcher(r) => {
                let Some(e) = table.iter().find(|e| e.pid == r.pid) else {
                    return found;
                };
                if e.start_time != r.start_time {
                    return found;
                }
                if !is_dead(e.state) {
                    found.push(r);
                }
                vec![r.pid]
            }
        };
        let mut seen: std::collections::HashSet<i32> = frontier.iter().copied().collect();
        while let Some(parent) = frontier.pop() {
            for e in table.iter().filter(|e| e.ppid == parent) {
                if seen.insert(e.pid) {
                    frontier.push(e.pid);
                    if !is_dead(e.state) {
                        found.push(ProcRef {
                            pid: e.pid,
                            start_time: e.start_time,
                        });
                    }
                }
            }
        }
        found
    }

    fn signal(&self, p: ProcRef, sig: Sig) {
        // A vanished pid, or one that now belongs to a later process, gets
        // nothing.
        if let Some(e) = stat_of(p.pid) {
            if e.start_time == p.start_time && !is_dead(e.state) {
                let _ = crate::util::sys::signal_pid(p.pid, sig);
            }
        }
    }

    fn signal_group(&self, pgid: i32, sig: Sig) {
        let _ = crate::util::sys::signal_group(pgid, sig);
    }

    fn reap_orphans(&self, except_pid: i32) {
        let Root::Own(me) = self.root else {
            return;
        };
        for e in Self::table() {
            if e.ppid == me && e.state == 'Z' && e.pid != except_pid {
                let _ = crate::util::sys::reap_nohang(e.pid);
            }
        }
    }
}

/// `/proc/<pid>/stat` as (state, ppid, starttime): fields 3, 4 and 22. The
/// command name (field 2) is in parentheses and may itself contain spaces and
/// `)`, so the fields are counted from the last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_stat(line: &str) -> Option<(char, i32, u64)> {
    let rest = line.get(line.rfind(')')? + 1..)?;
    let mut fields = rest.split_ascii_whitespace();
    let mut state = fields.next()?.chars();
    let (state, None) = (state.next()?, state.next()) else {
        return None;
    };
    let ppid = fields.next()?.parse().ok()?;
    // Field 5 is next; field 22 is 17 further on.
    let start_time = fields.nth(17)?.parse().ok()?;
    Some((state, ppid, start_time))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::process::ExitStatusExt;
    use std::rc::Rc;

    const L: i32 = 4000;
    const D: i32 = 4001;

    /// How a scripted process ends.
    #[derive(Debug, Clone, Copy)]
    enum Ends {
        /// By itself, this many ms after the start (or on a KILL before that).
        At(u64),
        /// This many ms after its first TERM, or at once on a KILL.
        TermAfter(u64),
        /// On a KILL only.
        KillOnly,
        /// Never, not even on a KILL.
        Never,
    }

    /// A signal the supervision sent.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sent {
        Group(i32, Sig),
        Proc(i32, Sig),
    }

    struct FakeProc {
        r: ProcRef,
        ends: Ends,
        /// The exit code it ends with by itself or after a TERM.
        code: i32,
        dies_at: Option<Instant>,
        by_kill: bool,
    }

    impl FakeProc {
        fn alive(&self, now: Instant) -> bool {
            !matches!(self.dies_at, Some(t) if t <= now)
        }

        fn receive(&mut self, sig: Sig, now: Instant) {
            if !self.alive(now) {
                return;
            }
            let at = match (sig, self.ends) {
                (_, Ends::Never) | (Sig::Term, Ends::At(_) | Ends::KillOnly) => return,
                (Sig::Term, Ends::TermAfter(ms)) => now + Duration::from_millis(ms),
                (Sig::Kill, _) => {
                    self.by_kill = true;
                    now
                }
            };
            self.dies_at = Some(self.dies_at.map_or(at, |t| t.min(at)));
        }
    }

    /// The scripted process table: the launcher is pid `L`, a descendant `D`.
    /// Every process is in the launcher's group.
    struct World {
        start: Instant,
        procs: Vec<FakeProc>,
        sent: Vec<(Duration, Sent)>,
        /// The launcher's handle fails every wait, as if someone else reaped it.
        wait_fails: bool,
    }

    type Shared = Rc<RefCell<World>>;

    fn world(procs: &[(i32, Ends, i32)]) -> Shared {
        let start = Instant::now();
        Rc::new(RefCell::new(World {
            start,
            procs: procs
                .iter()
                .map(|&(pid, ends, code)| FakeProc {
                    r: ProcRef {
                        pid,
                        start_time: 100 + pid as u64,
                    },
                    ends,
                    code,
                    dies_at: match ends {
                        Ends::At(ms) => Some(start + Duration::from_millis(ms)),
                        _ => None,
                    },
                    by_kill: false,
                })
                .collect(),
            sent: Vec::new(),
            wait_fails: false,
        }))
    }

    struct FakeLauncher(Shared);

    impl Launcher for FakeLauncher {
        fn pid(&self) -> i32 {
            L
        }

        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            let w = self.0.borrow();
            if w.wait_fails {
                return Err(io::Error::from_raw_os_error(10)); // ECHILD
            }
            let p = w.procs.iter().find(|p| p.r.pid == L).unwrap();
            if p.alive(Instant::now()) {
                return Ok(None);
            }
            Ok(Some(if p.by_kill {
                ExitStatus::from_raw(9)
            } else {
                ExitStatus::from_raw(p.code << 8)
            }))
        }
    }

    struct FakeOps(Shared);

    impl ProcOps for FakeOps {
        fn descendants(&self) -> Vec<ProcRef> {
            let now = Instant::now();
            let w = self.0.borrow();
            w.procs
                .iter()
                .filter(|p| p.alive(now))
                .map(|p| p.r)
                .collect()
        }

        fn signal(&self, r: ProcRef, sig: Sig) {
            let now = Instant::now();
            let mut w = self.0.borrow_mut();
            let at = now - w.start;
            let Some(p) = w.procs.iter_mut().find(|p| p.r == r && p.alive(now)) else {
                return;
            };
            p.receive(sig, now);
            w.sent.push((at, Sent::Proc(r.pid, sig)));
        }

        fn signal_group(&self, pgid: i32, sig: Sig) {
            let now = Instant::now();
            let mut w = self.0.borrow_mut();
            let at = now - w.start;
            w.sent.push((at, Sent::Group(pgid, sig)));
            for p in &mut w.procs {
                p.receive(sig, now);
            }
        }

        fn reap_orphans(&self, except_pid: i32) {
            assert_eq!(except_pid, L, "the launcher is left to its own handle");
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Short limits; a test changes what it is about.
    fn cfg() -> SuperviseCfg {
        SuperviseCfg {
            wall: Duration::from_secs(10),
            grace: ms(200),
            poll: ms(10),
            term_settle: ms(200),
            verify: ms(300),
        }
    }

    struct Run {
        got: Supervised,
        sent: Vec<(Duration, Sent)>,
        took: Duration,
    }

    /// Supervise `w`; if `term_at` is set, a manual kill arrives that many ms in.
    fn run_world(w: &Shared, cfg: &SuperviseCfg, term_at: Option<u64>) -> Run {
        let term = AtomicBool::new(false);
        let start = w.borrow().start;
        let got = std::thread::scope(|s| {
            if let Some(at) = term_at {
                let term = &term;
                s.spawn(move || {
                    std::thread::sleep(ms(at));
                    term.store(true, Ordering::SeqCst);
                });
            }
            supervise(
                &mut FakeLauncher(w.clone()),
                &FakeOps(w.clone()),
                &term,
                cfg,
            )
        });
        let took = start.elapsed();
        let sent = w.borrow().sent.clone();
        Run { got, sent, took }
    }

    fn run(procs: &[(i32, Ends, i32)], cfg: &SuperviseCfg, term_at: Option<u64>) -> Run {
        run_world(&world(procs), cfg, term_at)
    }

    fn calm() -> Observed {
        Observed::default()
    }

    #[test]
    fn natural_exit_with_no_descendants_returns_without_signals() {
        let r = run(&[(L, Ends::At(0), 0)], &cfg(), None);
        assert_eq!(r.sent, []);
        assert_eq!(
            r.got,
            Supervised {
                observation: Observation::Exited(0),
                observed: calm(),
                residual_count: 0,
            }
        );
        assert!(r.took < ms(200), "took {:?}", r.took);
    }

    #[test]
    fn natural_exit_waits_for_grace_before_term() {
        let c = SuperviseCfg {
            grace: ms(2000),
            ..cfg()
        };
        let r = run(&[(L, Ends::At(0), 0), (D, Ends::At(300), 0)], &c, None);
        assert_eq!(
            r.sent,
            [],
            "a descendant that ends within the grace is never asked"
        );
        assert_eq!(r.got.observation, Observation::Exited(0));
        assert_eq!(r.got.observed, calm());
        assert_eq!(r.got.residual_count, 0);
        assert!(r.took >= ms(300), "took {:?}", r.took);
        assert!(r.took < ms(300 + 300), "took {:?}", r.took);
    }

    #[test]
    fn natural_exit_terms_then_kills_lingering() {
        let c = cfg();
        let r = run(&[(L, Ends::At(0), 0), (D, Ends::KillOnly, 0)], &c, None);
        let kinds: Vec<Sent> = r.sent.iter().map(|s| s.1).collect();
        assert_eq!(
            kinds,
            [Sent::Proc(D, Sig::Term), Sent::Proc(D, Sig::Kill)],
            "only the leftover is signalled, one by one: the launcher's group is not"
        );
        let (term_at, kill_at) = (r.sent[0].0, r.sent[1].0);
        assert!(term_at >= c.grace, "TERM at {term_at:?}, before the grace");
        assert!(
            kill_at - term_at >= c.term_settle,
            "KILL {:?} after the TERM",
            kill_at - term_at
        );
        assert_eq!(r.got.observation, Observation::Exited(0));
        assert_eq!(r.got.observed, calm());
        assert_eq!(r.got.residual_count, 0);
    }

    #[test]
    fn wall_timeout_terms_the_launcher_group_first() {
        let c = SuperviseCfg {
            wall: ms(200),
            ..cfg()
        };
        let r = run(&[(L, Ends::TermAfter(100), 30)], &c, None);
        assert_eq!(r.sent[0].1, Sent::Group(L, Sig::Term));
        assert!(r.sent[0].0 >= c.wall, "TERM at {:?}", r.sent[0].0);
        assert_eq!(r.sent.len(), 1, "it ended within the grace: {:?}", r.sent);
        assert_eq!(r.got.observation, Observation::Exited(30));
        assert_eq!(
            r.got.observed,
            Observed {
                timed_out: true,
                ..calm()
            }
        );
        assert_eq!(r.got.residual_count, 0);
    }

    #[test]
    fn kill_waits_full_grace_before_sigkill() {
        let c = SuperviseCfg {
            wall: ms(100),
            grace: ms(400),
            ..cfg()
        };
        let r = run(&[(L, Ends::KillOnly, 0)], &c, None);
        let kinds: Vec<Sent> = r.sent.iter().map(|s| s.1).collect();
        assert_eq!(
            kinds,
            [Sent::Group(L, Sig::Term), Sent::Group(L, Sig::Kill)],
            "the KILL took the launcher, so nothing was left to signal one by one"
        );
        let (term_at, kill_at) = (r.sent[0].0, r.sent[1].0);
        assert!(term_at >= c.wall, "TERM at {term_at:?}");
        assert!(
            kill_at - term_at >= c.grace,
            "KILL {:?} after the TERM, before the grace was up",
            kill_at - term_at
        );
        assert_eq!(r.got.observation, Observation::Signaled);
        assert!(r.got.observed.timed_out);
        assert_eq!(r.got.residual_count, 0);
    }

    #[test]
    fn term_flag_marks_killed() {
        let r = run(&[(L, Ends::TermAfter(50), 30)], &cfg(), Some(100));
        assert_eq!(r.sent[0].1, Sent::Group(L, Sig::Term));
        assert!(r.sent[0].0 >= ms(100), "TERM at {:?}", r.sent[0].0);
        assert_eq!(r.got.observation, Observation::Exited(30));
        assert_eq!(
            r.got.observed,
            Observed {
                killed: true,
                ..calm()
            }
        );
    }

    #[test]
    fn term_flag_after_natural_exit_is_ignored() {
        // The launcher is done at once; its descendant keeps the clean-up going
        // past the moment the manual kill arrives.
        let c = SuperviseCfg {
            grace: ms(2000),
            ..cfg()
        };
        let r = run(&[(L, Ends::At(0), 0), (D, Ends::At(300), 0)], &c, Some(100));
        assert_eq!(r.got.observation, Observation::Exited(0));
        assert_eq!(
            r.got.observed,
            calm(),
            "C12: the launcher's result was already in"
        );
        assert_eq!(r.sent, [], "nor does the kill hurry the clean-up");
    }

    #[test]
    fn unkillable_descendant_is_residual() {
        let c = cfg();
        let r = run(&[(L, Ends::At(0), 0), (D, Ends::Never, 0)], &c, None);
        assert_eq!(r.got.observation, Observation::Exited(0));
        assert_eq!(r.got.residual_count, 1);
        assert_eq!(
            r.got.observed,
            Observed {
                residual: true,
                ..calm()
            }
        );
        let kinds: Vec<Sent> = r.sent.iter().map(|s| s.1).collect();
        assert_eq!(kinds, [Sent::Proc(D, Sig::Term), Sent::Proc(D, Sig::Kill)]);
        let bound = c.grace + c.term_settle + c.verify + ms(300);
        assert!(
            r.took >= c.grace + c.term_settle + c.verify,
            "took {:?}",
            r.took
        );
        assert!(r.took <= bound, "took {:?}, bound {bound:?}", r.took);
    }

    #[test]
    fn launcher_never_reaped_is_not_reaped_observation() {
        let c = SuperviseCfg {
            wall: ms(100),
            grace: ms(100),
            ..cfg()
        };
        let r = run(&[(L, Ends::Never, 0)], &c, None);
        assert_eq!(r.got.observation, Observation::NotReaped);
        assert!(r.got.observed.timed_out);
        assert_eq!(r.got.residual_count, 1);
        assert!(r.got.observed.residual);
        let kinds: Vec<Sent> = r.sent.iter().map(|s| s.1).collect();
        assert_eq!(
            kinds,
            [
                Sent::Group(L, Sig::Term),
                Sent::Group(L, Sig::Kill),
                Sent::Proc(L, Sig::Kill)
            ]
        );
    }

    #[test]
    fn a_launcher_that_cannot_be_waited_for_is_not_reaped_and_its_group_is_left_alone() {
        // Its handle fails (ECHILD: something else reaped it), so it is gone and
        // its pid may already belong to someone else: no group signal, no wall
        // timer or kill flag (C12), only the clean-up of what is left of the tree.
        let w = world(&[(D, Ends::At(100), 0)]);
        w.borrow_mut().wait_fails = true;
        let c = SuperviseCfg {
            wall: ms(50),
            ..cfg()
        };
        let r = run_world(&w, &c, Some(10));
        assert_eq!(r.got.observation, Observation::NotReaped);
        assert_eq!(r.got.observed, calm());
        assert_eq!(r.sent, []);
    }

    #[test]
    fn signals_are_only_sent_to_the_process_that_was_listed() {
        // A leftover whose pid was reused between the listing and the signal
        // gets nothing; the supervision does not care which, the fake checks it.
        let w = world(&[(L, Ends::At(0), 0)]);
        let ops = FakeOps(w.clone());
        ops.signal(
            ProcRef {
                pid: L,
                start_time: 1,
            },
            Sig::Kill,
        );
        assert_eq!(w.borrow().sent, []);
    }

    #[test]
    fn linux_proc_parses_stat_with_parens_in_comm() {
        let line = "123 (a) b) S 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 5 18446744073709551615";
        assert_eq!(parse_stat(line), Some(('S', 1, 98765)));
        let zombie = "77 (sh) Z 4000 77 77 0 -1 4227084 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0";
        assert_eq!(parse_stat(zombie), Some(('Z', 4000, 5)));
        for bad in [
            "",
            "123 a S 1",
            "123 (a) S 1 2 3",
            "123 (a) S x 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765",
            "123 (a) SS 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765",
            "123 (a) S 1 123 123 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 -5",
        ] {
            assert_eq!(parse_stat(bad), None, "{bad:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_parses_this_process_stat() {
        // Read-only: this process's own entry. Its state is the main thread's,
        // which is waiting for the tests, so only its being alive is checked.
        let line = std::fs::read_to_string("/proc/self/stat").unwrap();
        let (state, ppid, start_time) = parse_stat(&line).unwrap();
        assert!(matches!(state, 'R' | 'S' | 'D'), "{state}");
        assert_eq!(ppid as u32, std::os::unix::process::parent_id());
        assert!(start_time > 0);
    }

    #[test]
    fn a_task_gets_its_own_wall_and_grace_and_the_fixed_constants() {
        let t = CollectorTask {
            launcher_config: "/etc/geralt/launcher.json".into(),
            wall_timeout_secs: 600,
            grace_ms: 5000,
        };
        assert_eq!(
            SuperviseCfg::for_task(&t),
            SuperviseCfg {
                wall: Duration::from_secs(600),
                grace: ms(5000),
                poll: ms(50),
                term_settle: ms(1000),
                verify: ms(5000),
            }
        );
    }
}
