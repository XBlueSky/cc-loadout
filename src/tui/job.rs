use std::sync::mpsc::{self, Receiver};
use std::thread;

/// The repo scan produced by a background `Action::Rescan`, handed back to the
/// Profile view via `accept_scan` when the worker finishes.
pub struct ScanOutcome {
    pub roots: Vec<String>,
    pub repos: Vec<crate::profile::discover::RepoSignal>,
    pub suggested: Vec<crate::profile::discover::SuggestedProfile>,
    /// Repo paths matching no profile — computed on the job thread (post-merge)
    /// so `apply_scan` never re-walks the filesystem on the UI thread.
    pub uncovered: Vec<String>,
    /// Epoch seconds the scan finished — drives the by-plugin scan bar's age.
    pub scanned_at: i64,
}

/// The outcome of a background operation, surfaced to the UI.
#[derive(Default)]
pub struct JobResult {
    pub toast: String,
    pub needs_refresh: bool,
    pub draft: Option<crate::profile::config::Profiles>,
    /// Set by a background repo scan; delivered to the active view's `accept_scan`.
    pub scan: Option<ScanOutcome>,
    /// Set by a background drift recompute (the uncovered-repos set); delivered
    /// to the active view's `accept_uncovered`.
    pub uncovered: Option<Vec<String>>,
}

/// A running background operation. The UI shows a spinner with `label` until
/// `rx` yields a `JobResult`.
pub struct Job {
    pub label: String,
    pub started_ms: i64,
    pub rx: Receiver<JobResult>,
}

/// Spawn `f` on a background thread; the UI keeps animating until it completes.
pub fn spawn<F>(label: impl Into<String>, started_ms: i64, f: F) -> Job
where
    F: FnOnce() -> JobResult + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    Job {
        label: label.into(),
        started_ms,
        rx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_runs_and_delivers_result() {
        let job = spawn("test", 0, || JobResult {
            toast: "done".into(),
            needs_refresh: true,
            draft: None,
            scan: None,
            uncovered: None,
        });
        let r = job.rx.recv().unwrap();
        assert_eq!(r.toast, "done");
        assert!(r.needs_refresh);
    }
}
