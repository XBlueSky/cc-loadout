use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Held for the duration of a critical section; releases the flock on drop.
pub struct Lock {
    _file: File,
}

/// Acquire an exclusive advisory lock on `path` (creating it if needed).
pub fn acquire(path: &Path) -> Result<Lock> {
    let file = open_lock_file(path)?;
    file.lock_exclusive().context("acquiring exclusive lock")?;
    Ok(Lock { _file: file })
}

/// Open (creating it and its parent directory if needed) the file a lock is
/// taken on.
fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening lock file {}", path.display()))
}

/// Take the exclusive advisory lock on `path` only if nobody else holds it right
/// now. `Ok(None)` means another handle (this process's or another's) holds it;
/// every other failure — the file cannot be opened, an unexpected flock error —
/// is an `Err`, so a caller can tell "busy" from "broken".
pub fn try_acquire(path: &Path) -> Result<Option<Lock>> {
    let file = open_lock_file(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(Lock { _file: file })),
        // fs2 hands back the platform's "would block" error for a lock that is
        // held elsewhere; compare by OS error code, as `io::Error` is not `Eq`.
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(None),
        Err(e) => Err(e).context("acquiring exclusive lock"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_creates_lockfile_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".lock");
        let guard = acquire(&lock_path).unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }
    #[test]
    fn try_acquire_returns_none_while_another_handle_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".lock");
        let held = acquire(&lock_path).unwrap();
        // flock contends per open file description, so a second open in this
        // very process sees the lock as taken.
        assert!(try_acquire(&lock_path).unwrap().is_none());
        drop(held);
    }

    #[test]
    fn try_acquire_succeeds_after_holder_drops() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".lock");
        let held = acquire(&lock_path).unwrap();
        assert!(try_acquire(&lock_path).unwrap().is_none());
        drop(held);
        let again = try_acquire(&lock_path).unwrap();
        assert!(again.is_some(), "the lock is free once its holder is gone");
        // ...and while that second guard lives, a third try is refused.
        assert!(try_acquire(&lock_path).unwrap().is_none());
    }

    #[test]
    fn try_acquire_creates_the_lock_file_and_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("a").join("b").join(".lock");
        let guard = try_acquire(&lock_path).unwrap();
        assert!(guard.is_some());
        assert!(lock_path.exists());
    }

    #[test]
    fn try_acquire_reports_an_unopenable_lock_path_as_an_error() {
        // A directory where the lock file should be: opening it for writing
        // fails, and that is "broken", not "busy".
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".lock");
        std::fs::create_dir(&lock_path).unwrap();
        assert!(try_acquire(&lock_path).is_err());
    }

    #[test]
    fn acquire_still_blocks() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join(".lock");
        let (tx, rx) = mpsc::channel();
        let path = lock_path.clone();
        let holder = std::thread::spawn(move || {
            let guard = acquire(&path).unwrap();
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            drop(guard);
        });
        rx.recv().unwrap();
        let start = Instant::now();
        let guard = acquire(&lock_path).unwrap();
        let waited = start.elapsed();
        drop(guard);
        holder.join().unwrap();
        assert!(
            waited >= Duration::from_millis(250),
            "acquire returned after {waited:?}; it must keep blocking while the lock is held"
        );
    }
}
