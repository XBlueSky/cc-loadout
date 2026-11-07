//! The few Linux system calls the collector's process-tree supervision needs,
//! each behind a safe function that returns `io::Result` (errno becomes
//! [`std::io::Error::last_os_error`]). This is the only module of the crate that
//! may use `unsafe`, one block per function, and it exists on Linux only.
//!
//! None of these functions can address more than one process or group by
//! accident: a pid or pgid of 1 or less is refused before any call is made, so
//! `kill(0)`, `kill(-1)`, `killpg(0)`, `killpg(1)` and `waitpid(-1)` cannot be
//! sent from here.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

pub use crate::util::Sig;

impl Sig {
    fn raw(self) -> libc::c_int {
        match self {
            Sig::Term => libc::SIGTERM,
            Sig::Kill => libc::SIGKILL,
        }
    }
}

/// Refuse every pid (or pgid) that would mean "my group", "everyone" or init.
fn single_target(id: i32) -> io::Result<()> {
    if id <= 1 {
        Err(io::Error::from(io::ErrorKind::InvalidInput))
    } else {
        Ok(())
    }
}

/// Make this process the reaper of every orphan among its descendants
/// (`prctl(PR_SET_CHILD_SUBREAPER, 1)`), so nothing the launcher starts can
/// escape to init and out of sight. It lasts for the rest of the process.
pub fn become_child_subreaper() -> io::Result<()> {
    let one: libc::c_ulong = 1;
    let zero: libc::c_ulong = 0;
    // SAFETY: PR_SET_CHILD_SUBREAPER takes one integer argument and reads no
    // memory; the trailing arguments are unused and passed as zero.
    let r = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, one, zero, zero, zero) };
    if r == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Send `sig` to the one process `pid` (`kill(2)`). A process that is already
/// gone (`ESRCH`) counts as success.
pub fn signal_pid(pid: i32, sig: Sig) -> io::Result<()> {
    single_target(pid)?;
    // SAFETY: kill(2) takes two integers and touches no memory of this process;
    // `pid` is a single positive pid, never 0 or a negative group.
    let r = unsafe { libc::kill(pid, sig.raw()) };
    esrch_is_ok(r)
}

/// Send `sig` to every process of group `pgid` (`killpg(2)`). A group that no
/// longer has members (`ESRCH`) counts as success.
pub fn signal_group(pgid: i32, sig: Sig) -> io::Result<()> {
    single_target(pgid)?;
    // SAFETY: killpg(2) takes two integers and touches no memory of this
    // process; `pgid` is greater than 1, so it is never "my group" or init's.
    let r = unsafe { libc::killpg(pgid, sig.raw()) };
    esrch_is_ok(r)
}

fn esrch_is_ok(r: libc::c_int) -> io::Result<()> {
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(e)
    }
}

/// Reap child `pid` if it has ended (`waitpid(pid, WNOHANG)`): `true` when it
/// was reaped now, `false` while it is still running. Only ever this one pid,
/// never "any child".
pub fn reap_nohang(pid: i32) -> io::Result<bool> {
    single_target(pid)?;
    let mut status: libc::c_int = 0;
    // SAFETY: `status` is a live, writable c_int for the duration of the call;
    // `pid` is a single positive pid, so no other child can be reaped.
    let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    match r {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(false),
        _ => Ok(true),
    }
}

/// Set by the SIGTERM / SIGINT handler; never cleared.
static TERM_REQUESTED: AtomicBool = AtomicBool::new(false);

/// The handler: one atomic store, which is async-signal-safe, and nothing else.
extern "C" fn on_term(_sig: libc::c_int) {
    TERM_REQUESTED.store(true, Ordering::SeqCst);
}

/// Install a handler for SIGTERM and SIGINT that only sets the returned flag,
/// so that a manual kill of this process becomes a request to stop the attempt
/// rather than an exit that leaves the launcher's tree behind. Installing it
/// again changes nothing: it is the same handler and the same flag.
/// `SA_RESTART` keeps the signal from failing a blocking call elsewhere in the
/// process (the output pumps' reads); the supervision polls the flag.
pub fn install_term_flag() -> io::Result<&'static AtomicBool> {
    let handler: extern "C" fn(libc::c_int) = on_term;
    // SAFETY: `act` is fully initialised (all-zero is a valid `sigaction`, then
    // the mask is emptied and the handler and flags are set) and outlives both
    // calls; the old action is not requested. The handler only stores to a
    // static atomic, which is async-signal-safe.
    let failed = unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = handler as libc::sighandler_t;
        act.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut act.sa_mask);
        [libc::SIGTERM, libc::SIGINT]
            .into_iter()
            .any(|sig| libc::sigaction(sig, &act, std::ptr::null_mut()) == -1)
    };
    if failed {
        Err(io::Error::last_os_error())
    } else {
        Ok(&TERM_REQUESTED)
    }
}

#[cfg(test)]
mod tests {
    //! Only the guards: they return before any system call, so nothing here can
    //! reach a process. The calls themselves are proven by the binary tests, in a
    //! process of their own.
    use super::*;

    fn refused(r: io::Result<impl std::fmt::Debug>) -> bool {
        matches!(r, Err(e) if e.kind() == io::ErrorKind::InvalidInput)
    }

    #[test]
    fn signal_pid_refuses_everything_but_one_real_pid() {
        for pid in [1, 0, -1, -2, i32::MIN] {
            for sig in [Sig::Term, Sig::Kill] {
                assert!(refused(signal_pid(pid, sig)), "kill({pid}, {sig:?})");
            }
        }
    }

    #[test]
    fn signal_group_refuses_its_own_group_and_inits() {
        for pgid in [1, 0, -1, -2, i32::MIN] {
            for sig in [Sig::Term, Sig::Kill] {
                assert!(refused(signal_group(pgid, sig)), "killpg({pgid}, {sig:?})");
            }
        }
    }

    #[test]
    fn reap_nohang_never_waits_for_any_child() {
        for pid in [1, 0, -1, -2, i32::MIN] {
            assert!(refused(reap_nohang(pid)), "waitpid({pid})");
        }
    }

    #[test]
    fn signals_map_to_their_numbers() {
        assert_eq!(Sig::Term.raw(), libc::SIGTERM);
        assert_eq!(Sig::Kill.raw(), libc::SIGKILL);
    }
}
