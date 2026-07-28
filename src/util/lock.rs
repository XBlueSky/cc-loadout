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
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening lock file {}", path.display()))?;
    file.lock_exclusive().context("acquiring exclusive lock")?;
    Ok(Lock { _file: file })
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
}
