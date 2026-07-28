//! Shared utilities: atomic file writes and exclusive file locks.

pub mod atomicfile;
pub mod jsonmerge;
pub mod lock;

use std::path::PathBuf;

/// Resolve the absolute path of executable `name` on `$PATH` (like `which`).
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// Whether an executable named `claude` is resolvable on `$PATH`.
pub fn claude_on_path() -> bool {
    which("claude").is_some()
}

/// Replace this process with `claude --continue`. Returns only on failure.
pub fn exec_claude_continue() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("claude")
        .arg("--continue")
        .exec();
    Err(anyhow::Error::new(err).context("exec claude --continue"))
}

/// Retry a process spawn/exec that fails with `ETXTBSY` (os error 26) — a transient
/// race where a just-written executable is exec'd while another thread's `fork()`
/// still holds a write fd to it. Harmless in production (real binaries are never
/// busy); it eliminates a parallel-test flake. Gives up after ~0.5s.
pub fn retry_etxtbsy<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    for _ in 0..100 {
        match f() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => return other,
        }
    }
    f()
}
