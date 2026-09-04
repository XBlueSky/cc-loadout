use std::collections::BTreeMap;
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
    /// Number of repos where the vocabulary's `glob:` atoms could not all be
    /// answered because `discover::signals_for_repo`'s shared glob walk ran
    /// out of its dirent budget before finishing — those repos' glob-type
    /// `rule_hits` may be incomplete. The Rescan job folds this into the
    /// scan-completion toast before the outcome is built, so nothing reads it
    /// back off `ScanOutcome` today; kept on the struct (not just the toast
    /// string) as the source of truth for a future persistent indicator, same
    /// forward-scaffolding role as `JobResult.uncovered`.
    #[allow(dead_code)]
    pub budget_hits: usize,
}

/// The result of a background `Action::IndexAtoms` job: which atoms were
/// answered, and each repo's hit/miss for them. Delivered back to the Profile
/// view via `accept_index`.
pub struct IndexOutcome {
    pub atoms: Vec<String>,
    /// repo path -> (atom -> hit).
    pub hits: BTreeMap<String, BTreeMap<String, bool>>,
}

/// The outcome of a background operation, surfaced to the UI.
#[derive(Default)]
pub struct JobResult {
    pub toast: String,
    pub needs_refresh: bool,
    /// Set by a background repo scan; delivered to the active view's `accept_scan`.
    pub scan: Option<ScanOutcome>,
    /// Forward scaffolding: no current job sets this (a `Rescan`'s uncovered
    /// set flows through `ScanOutcome.uncovered` → `accept_scan` instead).
    /// Reserved for a future off-thread producer that isn't a full rescan.
    pub uncovered: Option<Vec<String>>,
    /// Set by a background `Action::IndexAtoms` job; delivered to the active
    /// view's `accept_index`.
    pub index: Option<IndexOutcome>,
}

/// A running background operation. The UI shows a spinner with `label` until
/// `rx` yields a `JobResult`.
pub struct Job {
    pub label: String,
    pub started_ms: i64,
    pub rx: Receiver<JobResult>,
}

/// Which kind of job a receiver moved into `App::detached` backs. A bare
/// `Receiver<JobResult>` can't tell `drain_jobs` what to recover if the
/// worker dies (e.g. panics) before sending a result — the channel just
/// disconnects with no `JobResult` at all. Most detached jobs (anything the
/// user Esc'd away from the modal slot) have no per-job view state riding on
/// them, so a dead worker is silently dropped, same as before. An
/// `Action::IndexAtoms` job is the one exception: `ProfileView.indexing`
/// (and the open Detail's `rules.indexing`) must be cleared even if the
/// worker never reports back, or every future `IndexAtoms` dispatch wedges
/// forever behind the `!self.indexing` guard.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DetachedKind {
    /// An Esc-detached job of any `Action` — nothing to recover on death.
    Generic,
    /// An `Action::IndexAtoms` job — `accept_index_failed` must run on death.
    IndexAtoms,
    /// A startup rebuild of a stale-version scan cache (Task 10's
    /// `App::new` `needs_rebuild` branch) — `accept_rebuild_failed` must run
    /// on death so the "index outdated — rebuilding…" banner doesn't stay up
    /// forever. Mirrors `IndexAtoms`: a silent `Generic` drop would leave the
    /// user staring at a banner for a rebuild that will never land.
    Rebuild,
}

/// A receiver held in `App::detached`, tagged with `kind` so `drain_jobs`
/// knows whether a disconnect (worker died without sending) needs recovery.
pub struct Detached {
    pub kind: DetachedKind,
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
            scan: None,
            uncovered: None,
            index: None,
        });
        let r = job.rx.recv().unwrap();
        assert_eq!(r.toast, "done");
        assert!(r.needs_refresh);
    }
}
