use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use crate::tui::accounts::AccountsView;
use crate::tui::ctx::AppCtx;
use crate::tui::overview::Overview;
use crate::tui::snapshot::Snapshot;
use crate::tui::theme;
use crate::tui::view::{Action, View};

/// Minimum time a background-job spinner stays on screen, even when the worker
/// finishes sooner. Keeps fast jobs (e.g. opening a profile's Detail) from
/// flashing the spinner for a frame — they read as a deliberate loading moment.
/// One knob: raise/lower to taste.
pub(crate) const MIN_SPINNER_MS: i64 = 2000;

/// The `Action::Rescan` completion toast: names the repo count, and appends a
/// warning when `budget_hits` repos hit the glob-walk budget (see
/// `ScanOutcome::budget_hits`) — those repos' `glob:` rule_hits may be
/// incomplete until a follow-up scan/index run.
fn rescan_toast(repo_count: usize, budget_hits: usize) -> String {
    if budget_hits > 0 {
        format!("scanned {repo_count} repos \u{b7} {budget_hits} repos hit walk budget")
    } else {
        format!("scanned {repo_count} repos")
    }
}

/// Build the background job for a full repo scan: walk `roots`, compute
/// rule_hits/uncovered from the freshly-indexed signals, and best-effort
/// persist the result into the scan cache. Shared by `Action::Rescan` (the
/// `s` key) and `App::new`'s startup rebuild of a stale-version scan cache
/// (Task 10) — both need the identical computation, so there is exactly one
/// place that knows how a scan turns into a `ScanOutcome` + cache write.
fn rescan_job(
    roots: Vec<String>,
    working: crate::profile::config::Profiles,
    data_root: std::path::PathBuf,
    now_ms: i64,
) -> crate::tui::job::Job {
    let label = if roots.len() == 1 {
        format!("scanning {}", roots[0])
    } else {
        format!("scanning {} roots", roots.len())
    };
    crate::tui::job::spawn(label, now_ms, move || {
        let vocab = crate::profile::signal_detect::vocabulary(&working);
        let (repos, budget_hits) =
            crate::profile::discover::scan_repo_signals_with_budget_hits(&roots, 6, &vocab);
        let suggested = crate::profile::discover::suggest_profiles(&repos);
        // Compute the uncovered drift here (post-merge, from the freshly-indexed
        // signals) so the UI thread never re-walks every repo when folding the
        // result. A fresh scan indexes the full current-rule vocabulary, so
        // every rule is index-answerable by construction — `pending` must be
        // false (debug-asserted below; if it somehow isn't, the signal
        // evaluator still degrades gracefully by leaving undecided repos out
        // of `uncovered` rather than guessing).
        let mut merged = working.clone();
        for sp in &suggested {
            merged.profiles.entry(sp.name.clone()).or_insert_with(|| {
                crate::profile::author::profile_from(Vec::new(), &sp.shared_signals)
            });
        }
        let (uncovered, pending) = crate::profile::drift::uncovered_from_signals(&repos, &merged);
        debug_assert!(
            !pending,
            "a fresh scan indexes the full current-rule vocabulary; \
             uncovered_from_signals must be decisive right after Rescan"
        );
        let scanned_at = crate::now_epoch();
        // Best-effort: persist so a reopen shows counts + drift without a
        // re-walk. A failed cache write must not fail the scan.
        let _ = crate::profile::scan_cache::save(
            &data_root,
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: roots.clone(),
                repos: repos.clone(),
                uncovered: Some(uncovered.clone()),
                scanned_at,
            },
        );
        crate::tui::job::JobResult {
            toast: rescan_toast(repos.len(), budget_hits),
            needs_refresh: false,
            draft: None,
            scan: Some(crate::tui::job::ScanOutcome {
                roots,
                repos,
                suggested,
                uncovered,
                scanned_at,
                budget_hits,
            }),
            uncovered: None,
            index: None,
        }
    })
}

pub struct App {
    tabs: Vec<Box<dyn View>>,
    active: usize,
    pub should_quit: bool,
    pub relaunch: bool,
    pub toast: Option<String>,
    /// Wall-clock time (ms since epoch) when the current toast was set.
    toast_at_ms: Option<i64>,
    pub frame: u64,
    pub job: Option<crate::tui::job::Job>,
    /// Receivers for jobs that were detached via Esc, or that always run
    /// detached (e.g. `Action::IndexAtoms`). Their results are still
    /// delivered when the worker finishes; each entry is tagged with its
    /// `DetachedKind` so `drain_jobs` knows what to recover if the worker
    /// dies without sending a result.
    pub(crate) detached: Vec<crate::tui::job::Detached>,
    ctx: AppCtx,
    snap: Snapshot,
}

impl App {
    pub fn new(ctx: AppCtx, initial_tab: usize) -> Result<App> {
        let snap = Snapshot::load(&ctx)?;
        // Stage B: build ONLY the plugin inventory at startup — no repo scan.
        // Scanning is an explicit, user-triggered action (`s`) in the Profile
        // view, so the board never opens having silently walked the filesystem.
        let claude_available = crate::util::claude_on_path();
        let inv = crate::profile::discover::build_inventory_no_scan(&ctx.registry_path);
        let cfg_existed = ctx.cfg_path.exists();
        // Load an existing config; a missing/unreadable file yields an empty
        // working config (first-run) rather than a panic or an eager scan.
        let working = if cfg_existed {
            crate::profile::config::load(&ctx.cfg_path).unwrap_or_default()
        } else {
            crate::profile::config::Profiles::default()
        };
        // Suggested scan roots for the explicit `s` action: from a loaded config,
        // else the parent of cwd as a non-committed suggestion.
        let scan_roots = if working.scan_roots.is_empty() {
            vec![ctx
                .cwd
                .parent()
                .unwrap_or(ctx.cwd.as_path())
                .to_string_lossy()
                .into_owned()]
        } else {
            working.scan_roots.clone()
        };
        // Bug 2: seed repo signals from the last scan cache when its roots match
        // the current scan roots, so the board shows counts on reopen without a
        // filesystem walk (Stage B keeps startup walk-free). The uncovered drift
        // was computed at scan time and cached alongside the repos — seed it too,
        // so the board shows "⚠ N repos match nothing" WITHOUT re-running
        // detection (a per-repo walk) on the startup critical path.
        let mut scanned_at = None;
        let mut cached_repos = Vec::new();
        let mut cached_uncovered = Vec::new();
        // A cached `uncovered: None` means the field was never computed (e.g. a
        // stale-version cache predates the atom index, so a signal-based
        // recompute over it would land every repo Unknown) — seed uncovered
        // empty and mark it pending rather than guessing.
        let mut uncovered_pending = false;
        // Task 10: a cache stamped with an older schema version needs its
        // rule_hits rebuilt from scratch (a v1 cache has none at all) before
        // any signal-based recompute can trust it. Purely version-driven —
        // independent of whether `uncovered` above happened to be `Some` or
        // `None` — so the two seeds can each be true or false on their own
        // without ever causing a second spawn.
        let mut needs_rebuild = false;
        if let Some(cache) = crate::profile::scan_cache::load(&ctx.data_root) {
            if cache.roots == scan_roots {
                cached_repos = cache.repos;
                match cache.uncovered {
                    Some(u) => cached_uncovered = u,
                    None => uncovered_pending = !cached_repos.is_empty(),
                }
                scanned_at = Some(cache.scanned_at);
                needs_rebuild = cache.version < crate::profile::scan_cache::SCAN_CACHE_VERSION
                    && !cached_repos.is_empty();
            }
        }
        // A stale-version cache walks itself back onto the current schema on a
        // detached background thread — startup itself never blocks on it
        // (Stage B stays walk-free): `App::new` only ever reads the cache
        // file above. `rescan_job` is the SAME implementation the `s` key
        // dispatches, so the rebuilt cache is exactly what an explicit
        // rescan would have produced.
        let mut detached: Vec<crate::tui::job::Detached> = Vec::new();
        if needs_rebuild {
            let job = rescan_job(
                scan_roots.clone(),
                working.clone(),
                ctx.data_root.clone(),
                crate::now_epoch() * 1000,
            );
            detached.push(crate::tui::job::Detached {
                kind: crate::tui::job::DetachedKind::Rebuild,
                rx: job.rx,
            });
        }
        let tabs: Vec<Box<dyn View>> = vec![
            Box::new(Overview::new()),
            Box::new(AccountsView::new()),
            Box::new(crate::tui::schedule::ScheduleView::new()),
            Box::new(
                // Construct with a walk-free (empty-repo) inventory — the
                // constructor itself never touches disk — then seed the cached
                // repos and uncovered drift via builders that do NO filesystem
                // I/O either.
                crate::tui::profile::ProfileView::new(inv, working, claude_available, !cfg_existed)
                    .with_scan_roots(scan_roots)
                    .with_scanned_at(scanned_at)
                    .with_scan_repos(cached_repos)
                    .with_uncovered(cached_uncovered)
                    .with_uncovered_pending(uncovered_pending)
                    .with_index_rebuilding(needs_rebuild),
            ),
            Box::new(crate::tui::tasks::TasksView::new()),
        ];
        let active = initial_tab.min(tabs.len().saturating_sub(1));
        let app = App {
            tabs,
            active,
            should_quit: false,
            relaunch: false,
            toast: None,
            toast_at_ms: None,
            frame: 0,
            job: None,
            detached,
            ctx,
            snap,
        };
        Ok(app)
    }

    /// Returns the currently-selected tab index. Used by unit tests to assert tab
    /// cycling; the bin drives the tab strip by index internally.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Test-visible: whether the active view claims `code` (used to assert that
    /// a sub-view entered a state that handles a key, e.g. Detail rename).
    #[cfg(test)]
    pub(crate) fn active_claims_key(&self, code: ratatui::crossterm::event::KeyCode) -> bool {
        self.tabs[self.active].claims_key(code)
    }

    /// Advance the animation frame counter by one (wrapping). Called by the event
    /// loop on each poll timeout so that breathing and spinner animations progress.
    pub fn tick(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Returns `true` when the UI needs continuous redraws (≈100 ms cadence).
    /// False means the loop can idle at 1 s between redraws.
    pub fn animating(&self) -> bool {
        self.job.is_some() || self.toast.is_some() || !self.detached.is_empty()
    }

    /// Set the toast message, recording the wall-clock timestamp for auto-fade.
    pub(crate) fn set_toast(&mut self, msg: String) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.toast = Some(msg);
        self.toast_at_ms = Some(now_ms);
    }

    pub(crate) fn refresh(&mut self) -> Result<()> {
        self.snap = Snapshot::load(&self.ctx)?;
        Ok(())
    }

    /// Autosave the active view's config when it has unsaved edits — a small,
    /// synchronous atomic write (not routed through the single job slot). A write
    /// failure surfaces a toast so a save is never silently lost.
    fn persist_active_config(&mut self) {
        if let Some(cfg) = self.tabs[self.active].dirty_config() {
            if let Err(e) = crate::profile::author::write_profiles_quiet(&self.ctx.cfg_path, &cfg) {
                self.set_toast(format!("save failed: {e}"));
            }
        }
        self.persist_active_uncovered();
    }

    /// Persist the active view's uncovered-repos set into the scan cache when it
    /// changed. This keeps the cache the authoritative uncovered source across
    /// sessions no matter which path recomputed it (scan, rule edit, membership
    /// change) — so the next startup seeds a fresh value without re-walking.
    /// Best-effort: only updates the `uncovered` field of an existing cache whose
    /// roots still match; a missing/mismatched cache is left for the next scan to
    /// write, and any I/O error is silently ignored (a stale drift is harmless).
    fn persist_active_uncovered(&mut self) {
        let Some(uncovered) = self.tabs[self.active].dirty_uncovered() else {
            return;
        };
        if let Some(mut cache) = crate::profile::scan_cache::load(&self.ctx.data_root) {
            if cache.uncovered.as_deref() != Some(uncovered.as_slice()) {
                cache.uncovered = Some(uncovered);
                let _ = crate::profile::scan_cache::save(&self.ctx.data_root, &cache);
            }
        }
    }

    /// Drain the active job and all detached receivers, delivering any
    /// completed results as toasts and clearing finished jobs. Called each
    /// iteration of the event loop.
    pub(crate) fn drain_jobs(&mut self, now_ms: i64) -> Result<()> {
        use std::sync::mpsc::TryRecvError;
        // active job — extract the poll result before releasing the borrow so
        // that we can call &mut self methods (set_toast, refresh) afterward.
        enum ActivePoll {
            Done {
                toast: String,
                refresh: bool,
                draft: Option<Box<crate::profile::config::Profiles>>,
                scan: Box<Option<crate::tui::job::ScanOutcome>>,
                uncovered: Option<Vec<String>>,
                index: Option<crate::tui::job::IndexOutcome>,
            },
            Aborted,
            Pending,
        }
        let active_poll = if let Some(job) = &self.job {
            // Minimum spinner duration: even if the worker already finished, keep
            // the spinner up until MIN_SPINNER_MS has elapsed so a fast job reads
            // as a deliberate loading moment instead of a flash.
            if now_ms.saturating_sub(job.started_ms) < MIN_SPINNER_MS {
                ActivePoll::Pending
            } else {
                match job.rx.try_recv() {
                    Ok(result) => ActivePoll::Done {
                        toast: result.toast,
                        refresh: result.needs_refresh,
                        draft: result.draft.map(Box::new),
                        scan: Box::new(result.scan),
                        uncovered: result.uncovered,
                        index: result.index,
                    },
                    Err(TryRecvError::Disconnected) => ActivePoll::Aborted,
                    Err(TryRecvError::Empty) => ActivePoll::Pending,
                }
            }
        } else {
            ActivePoll::Pending
        };
        match active_poll {
            ActivePoll::Done {
                toast,
                refresh,
                draft,
                scan,
                uncovered,
                index,
            } => {
                self.job = None;
                // These callbacks are consumed only by the Profile view, so route
                // them to it explicitly — never to whatever tab happens to be
                // active — or a result landing while the user is on another tab
                // (or a detached/startup job) would be silently dropped.
                if let Some(p) = draft {
                    self.tabs[crate::tui::TAB_PROFILE].accept_draft(*p);
                }
                if let Some(o) = *scan {
                    self.tabs[crate::tui::TAB_PROFILE].accept_scan(o);
                }
                if let Some(u) = uncovered {
                    self.tabs[crate::tui::TAB_PROFILE].accept_uncovered(u);
                }
                if let Some(o) = index {
                    self.tabs[crate::tui::TAB_PROFILE].accept_index(o);
                }
                // An empty toast (e.g. a silent drift recompute) shows nothing.
                if !toast.is_empty() {
                    self.set_toast(toast);
                }
                if refresh {
                    self.refresh()?;
                }
            }
            ActivePoll::Aborted => {
                self.job = None;
                self.set_toast("operation aborted".to_string());
            }
            ActivePoll::Pending => {}
        }
        // detached receivers: deliver their result (or recover on disconnect), keep pending.
        // Collect into local vecs first so we don't hold a mutable borrow on
        // self.detached while calling self.set_toast later.
        let mut still_pending: Vec<crate::tui::job::Detached> = Vec::new();
        let mut completed_toasts: Vec<String> = Vec::new();
        let mut completed_drafts: Vec<crate::profile::config::Profiles> = Vec::new();
        let mut completed_scans: Vec<crate::tui::job::ScanOutcome> = Vec::new();
        let mut completed_uncovered: Vec<Vec<String>> = Vec::new();
        let mut completed_index: Vec<crate::tui::job::IndexOutcome> = Vec::new();
        // An IndexAtoms/Rebuild worker that disconnected without sending a
        // result (e.g. it panicked) — recovery is routed after the loop,
        // once the borrow on self.detached is released. Most detached jobs
        // (Generic) have no per-job view state to recover, so a disconnect
        // there is still silently dropped, same as before this fix.
        let mut index_job_died = false;
        let mut rebuild_job_died = false;
        let mut refresh_needed = false;
        for job in self.detached.drain(..) {
            match job.rx.try_recv() {
                Ok(result) => {
                    if result.needs_refresh {
                        refresh_needed = true;
                    }
                    if let Some(p) = result.draft {
                        completed_drafts.push(p);
                    }
                    if let Some(o) = result.scan {
                        completed_scans.push(o);
                    }
                    if let Some(u) = result.uncovered {
                        completed_uncovered.push(u);
                    }
                    if let Some(o) = result.index {
                        completed_index.push(o);
                    }
                    completed_toasts.push(result.toast);
                }
                Err(TryRecvError::Disconnected) => match job.kind {
                    crate::tui::job::DetachedKind::IndexAtoms => index_job_died = true,
                    crate::tui::job::DetachedKind::Rebuild => rebuild_job_died = true,
                    crate::tui::job::DetachedKind::Generic => {
                        // Generic: worker gone, nothing to show (unchanged).
                    }
                },
                Err(TryRecvError::Empty) => still_pending.push(job), // keep polling
            }
        }
        self.detached = still_pending;
        // Route drafts/scans/uncovered/index from detached jobs to the Profile
        // view (their only consumer), not the active tab — a detached job
        // commonly completes while the user has tabbed away.
        for p in completed_drafts {
            self.tabs[crate::tui::TAB_PROFILE].accept_draft(p);
        }
        for o in completed_scans {
            self.tabs[crate::tui::TAB_PROFILE].accept_scan(o);
        }
        for u in completed_uncovered {
            self.tabs[crate::tui::TAB_PROFILE].accept_uncovered(u);
        }
        for o in completed_index {
            self.tabs[crate::tui::TAB_PROFILE].accept_index(o);
        }
        if index_job_died {
            // Mirrors the modal path's ActivePoll::Aborted → "operation
            // aborted" toast: a dead worker is a real, user-visible event,
            // not something to recover silently — even though the flag
            // itself clears without the user having to do anything.
            self.tabs[crate::tui::TAB_PROFILE].accept_index_failed();
            completed_toasts.push("indexing aborted".to_string());
        }
        if rebuild_job_died {
            // Same shape as the IndexAtoms recovery above: clear the
            // "index outdated — rebuilding…" banner (it would otherwise
            // never clear) and surface a user-visible toast — the cache
            // stays stale until the user rescans explicitly.
            self.tabs[crate::tui::TAB_PROFILE].accept_rebuild_failed();
            completed_toasts.push("index rebuild aborted \u{b7} press s to rescan".to_string());
        }
        // Apply the last toast (if multiple completed, only the last is shown).
        for toast in completed_toasts {
            if !toast.is_empty() {
                self.set_toast(toast);
            }
        }
        if refresh_needed {
            self.refresh()?;
        }
        self.persist_active_config();
        Ok(())
    }

    fn apply_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::Refresh => {
                self.refresh()?;
            }
            Action::Switch(alias) => {
                let store = self.ctx.store.clone();
                let claude = self.ctx.claude.clone();
                let home = self.ctx.home.clone();
                let now = crate::now_epoch();
                self.job = Some(crate::tui::job::spawn(
                    format!("switching to '{alias}'"),
                    crate::now_epoch() * 1000,
                    move || match crate::account::swap::switch(&store, &claude, &home, &alias, now)
                    {
                        Ok(out) => crate::tui::job::JobResult {
                            toast: match out.warning {
                                Some(w) => format!("switched to '{}' (warning: {w})", out.to),
                                None => format!("switched to '{}'", out.to),
                            },
                            needs_refresh: true,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                        Err(e) => crate::tui::job::JobResult {
                            toast: format!("switch failed: {e}"),
                            needs_refresh: false,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                    },
                ));
            }
            Action::Prime(alias) => {
                let store = self.ctx.store.clone();
                let claude = self.ctx.claude.clone();
                let home = self.ctx.home.clone();
                let now = crate::now_epoch();
                self.job = Some(crate::tui::job::spawn(
                    format!("priming '{alias}'"),
                    crate::now_epoch() * 1000,
                    move || match crate::account::prime::prime(
                        &store, &claude, &home, &alias, "ok", now,
                    ) {
                        Ok(crate::account::prime::PrimeOutcome::Primed) => {
                            crate::tui::job::JobResult {
                                toast: format!("primed '{alias}'"),
                                needs_refresh: true,
                                draft: None,
                                scan: None,
                                uncovered: None,
                                index: None,
                            }
                        }
                        Ok(crate::account::prime::PrimeOutcome::SkippedActive) => {
                            crate::tui::job::JobResult {
                                toast: format!("'{alias}' is active — prime skipped"),
                                needs_refresh: false,
                                draft: None,
                                scan: None,
                                uncovered: None,
                                index: None,
                            }
                        }
                        Err(e) => crate::tui::job::JobResult {
                            toast: format!("prime failed: {e}"),
                            needs_refresh: false,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                    },
                ));
            }
            Action::RemoveAccount(alias) => {
                let store = self.ctx.store.clone();
                self.job = Some(crate::tui::job::spawn(
                    format!("removing '{alias}'"),
                    crate::now_epoch() * 1000,
                    move || match crate::account::remove(&store, &alias) {
                        Ok(()) => crate::tui::job::JobResult {
                            toast: format!("removed '{alias}'"),
                            needs_refresh: true,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                        Err(e) => crate::tui::job::JobResult {
                            toast: format!("remove failed: {e}"),
                            needs_refresh: false,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                    },
                ));
            }
            Action::RelaunchClaude => {
                self.relaunch = true;
            }
            Action::Commit {
                cfg,
                repos,
                expected,
            } => {
                let cfg_path = self.ctx.cfg_path.clone();
                let settings_path = crate::profile::apply::global_settings_path(&self.ctx.claude);
                let data_root = self.ctx.data_root.clone();
                let now = crate::now_epoch();
                self.job = Some(crate::tui::job::spawn(
                    format!("writing {}", self.ctx.cfg_path.display()),
                    crate::now_epoch() * 1000,
                    move || match crate::profile::commit::commit(
                        &cfg_path,
                        &settings_path,
                        &data_root,
                        &cfg,
                        &repos,
                        &expected,
                        now,
                    ) {
                        Ok(r) => {
                            let mut toast = format!(
                                "wrote {} · global synced · {} repos applied",
                                r.profiles_path.display(),
                                r.repos_applied
                            );
                            if r.diverged > 0 {
                                toast.push_str(&format!(
                                    " · {} matched differently than preview",
                                    r.diverged
                                ));
                            }
                            // Fold the freshly re-detected repos back into the
                            // in-memory inventory via the same accept_index
                            // path IndexAtoms uses, tagged with an EMPTY atoms
                            // list — this is a repo-signal refresh, not the
                            // completion of a real atom-indexing batch (see
                            // accept_index's own doc comment on why that
                            // distinction matters: an IndexAtoms job can be
                            // genuinely in flight at the same time, since
                            // Commit uses the modal job slot and IndexAtoms
                            // runs detached). Without this, reopening Apply
                            // right after a commit would show the stale
                            // pre-commit preview until the next explicit
                            // rescan.
                            let index = if r.fresh_signals.is_empty() {
                                None
                            } else {
                                let hits: std::collections::BTreeMap<
                                    String,
                                    std::collections::BTreeMap<String, bool>,
                                > = r
                                    .fresh_signals
                                    .iter()
                                    .map(|sig| (sig.path.clone(), sig.rule_hits.clone()))
                                    .collect();
                                Some(crate::tui::job::IndexOutcome {
                                    atoms: vec![],
                                    hits,
                                })
                            };
                            crate::tui::job::JobResult {
                                toast,
                                needs_refresh: true,
                                draft: None,
                                scan: None,
                                uncovered: None,
                                index,
                            }
                        }
                        Err(e) => crate::tui::job::JobResult {
                            toast: format!("commit failed: {e}"),
                            needs_refresh: false,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                    },
                ));
            }
            Action::WriteSchedule(sched) => {
                let data_root = self.ctx.data_root.clone();
                let home = self.ctx.home.clone();
                self.job = Some(crate::tui::job::spawn(
                    "writing schedule",
                    crate::now_epoch() * 1000,
                    move || {
                        let crontab = match crate::account::crontab::resolve_bin() {
                            Ok(p) => p,
                            Err(e) => {
                                return crate::tui::job::JobResult {
                                    toast: format!("schedule NOT installed: {e}"),
                                    needs_refresh: true,
                                    ..Default::default()
                                }
                            }
                        };
                        match crate::task::ops::write_prime_schedule(
                            &crontab, &data_root, &home, &sched,
                        ) {
                            Ok(()) => crate::tui::job::JobResult {
                                toast: "schedule written; crontab updated".to_string(),
                                needs_refresh: true,
                                ..Default::default()
                            },
                            // The schedule is NOT saved when the crontab can't be
                            // installed (apply-cron-before-save), so this is a real
                            // "nothing happened" — surfaced instead of silently lost.
                            Err(e) => crate::tui::job::JobResult {
                                toast: format!("schedule NOT installed: {e}"),
                                needs_refresh: true,
                                ..Default::default()
                            },
                        }
                    },
                ));
            }
            Action::DraftWithClaude { inv, scan_roots } => {
                self.spawn_draft_job(inv, scan_roots, crate::util::which("claude"));
            }
            Action::Rescan { roots, working } => {
                let data_root = self.ctx.data_root.clone();
                self.job = Some(rescan_job(
                    roots,
                    working,
                    data_root,
                    crate::now_epoch() * 1000,
                ));
            }
            Action::IndexAtoms { atoms, repos } => {
                // Real disk I/O (file/content/kw stats + one `globs_exist`
                // walk per repo) — MUST run detached, never through the modal
                // `self.job` slot, or committing a new rule would freeze
                // every other key until the walk finishes.
                let label = if atoms.len() == 1 {
                    format!("indexing {}", atoms[0])
                } else {
                    format!("indexing {} patterns", atoms.len())
                };
                let data_root = self.ctx.data_root.clone();
                let job = crate::tui::job::spawn(label, crate::now_epoch() * 1000, move || {
                    let atom_set: std::collections::BTreeSet<String> =
                        atoms.iter().cloned().collect();
                    let mut hits: std::collections::BTreeMap<
                        String,
                        std::collections::BTreeMap<String, bool>,
                    > = std::collections::BTreeMap::new();
                    for repo_path in &repos {
                        // Budget exhaustion isn't surfaced from this job (only
                        // the Rescan job reports ScanOutcome::budget_hits) —
                        // an atom this walk can't finish just stays unindexed
                        // and gets picked up by the next scan/index batch.
                        let (repo_hits, _exhausted) = crate::profile::discover::answer_atoms(
                            std::path::Path::new(repo_path),
                            &atom_set,
                            crate::profile::detect::GLOB_WALK_BUDGET,
                        );
                        hits.insert(repo_path.clone(), repo_hits);
                    }
                    // Best-effort load-merge-save into the scan cache, mirroring
                    // Rescan's cache write above: a killed TUI at worst loses
                    // this in-flight batch, never corrupts the cache.
                    if let Some(mut cache) = crate::profile::scan_cache::load(&data_root) {
                        for repo in &mut cache.repos {
                            if let Some(repo_hits) = hits.get(&repo.path) {
                                for (atom, hit) in repo_hits {
                                    repo.rule_hits.insert(atom.clone(), *hit);
                                }
                            }
                        }
                        let _ = crate::profile::scan_cache::save(&data_root, &cache);
                    }
                    let toast = if atoms.len() == 1 {
                        let atom = &atoms[0];
                        let n = hits
                            .values()
                            .filter(|h| h.get(atom).copied().unwrap_or(false))
                            .count();
                        format!("indexed {atom} \u{b7} {n} repos match")
                    } else {
                        format!("indexed {} patterns", atoms.len())
                    };
                    crate::tui::job::JobResult {
                        toast,
                        needs_refresh: false,
                        index: Some(crate::tui::job::IndexOutcome { atoms, hits }),
                        ..Default::default()
                    }
                });
                // Detached, not modal: the whole point of this feature is that
                // committing a new rule atom never swallows the keyboard.
                // Tagged IndexAtoms so drain_jobs can recover the view's
                // indexing flag if the worker dies without a result.
                self.detached.push(crate::tui::job::Detached {
                    kind: crate::tui::job::DetachedKind::IndexAtoms,
                    rx: job.rx,
                });
            }
            Action::RunTask(id) => {
                let store = self.ctx.store.clone();
                let data_root = self.ctx.data_root.clone();
                let home = self.ctx.home.clone();
                let live_plugins = self
                    .ctx
                    .claude
                    .credentials
                    .parent()
                    .map(|d| d.join("plugins"))
                    .unwrap_or_else(|| home.join(".claude").join("plugins"));
                self.job = Some(crate::tui::job::spawn(
                    format!("running task {id}"),
                    crate::now_epoch() * 1000,
                    move || match crate::task::run::run_task(
                        &store,
                        &data_root,
                        &home,
                        &live_plugins,
                        &id,
                        crate::now_epoch(),
                    ) {
                        Ok(out) => crate::tui::job::JobResult {
                            toast: format!("task '{id}' → {out:?}"),
                            needs_refresh: true,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                        Err(e) => crate::tui::job::JobResult {
                            toast: format!("task '{id}' failed: {e}"),
                            needs_refresh: false,
                            draft: None,
                            scan: None,
                            uncovered: None,
                            index: None,
                        },
                    },
                ));
            }
            Action::RemoveTask(id) => {
                let data_root = self.ctx.data_root.clone();
                let home = self.ctx.home.clone();
                self.job = Some(crate::tui::job::spawn(
                    format!("removing task {id}"),
                    crate::now_epoch() * 1000,
                    move || {
                        let crontab = match crate::account::crontab::resolve_bin() {
                            Ok(p) => p,
                            Err(e) => {
                                return crate::tui::job::JobResult {
                                    toast: format!("remove failed: {e}"),
                                    needs_refresh: true,
                                    ..Default::default()
                                }
                            }
                        };
                        match crate::task::ops::remove(&crontab, &data_root, &home, &id) {
                            Ok(()) => crate::tui::job::JobResult {
                                toast: format!("removed task '{id}'"),
                                needs_refresh: true,
                                ..Default::default()
                            },
                            Err(e) => crate::tui::job::JobResult {
                                toast: format!("remove failed: {e}"),
                                needs_refresh: true,
                                ..Default::default()
                            },
                        }
                    },
                ));
            }
        }
        Ok(())
    }

    /// Shared implementation for `Action::DraftWithClaude`. Accepts an explicit
    /// `claude_bin` so that unit tests can inject `None` without touching `PATH`.
    pub(crate) fn spawn_draft_job(
        &mut self,
        inv: crate::profile::discover::Inventory,
        scan_roots: Vec<String>,
        claude_bin: Option<std::path::PathBuf>,
    ) {
        let bin = match claude_bin {
            Some(b) => b,
            None => {
                self.set_toast("claude not found — using scan draft".to_string());
                return;
            }
        };
        // If we have no repo context yet, the depth-6 scan happens on the job
        // thread (below) rather than freezing the UI before the spinner shows.
        let label = if inv.repos.is_empty() && !scan_roots.is_empty() {
            "scanning + asking Claude…"
        } else {
            "asking Claude…"
        };
        self.job = Some(crate::tui::job::spawn(
            label,
            crate::now_epoch() * 1000,
            move || {
                let mut inv = inv;
                if inv.repos.is_empty() && !scan_roots.is_empty() {
                    // No loaded profiles config is available on this path (the
                    // draft flow runs before any profile assignment exists), so
                    // index against the marker/glob defaults only.
                    let vocab = crate::profile::signal_detect::vocabulary(
                        &crate::profile::config::Profiles::default(),
                    );
                    inv.repos = crate::profile::discover::scan_repo_signals(&scan_roots, 6, &vocab);
                    inv.suggested_profiles = crate::profile::discover::suggest_profiles(&inv.repos);
                }
                match crate::profile::ai::draft_with_claude(
                    &inv,
                    scan_roots,
                    &bin,
                    crate::profile::ai::DEFAULT_MODEL,
                    60,
                ) {
                    Ok(cfg) => crate::tui::job::JobResult {
                        toast: "Claude drafted your profiles — review, then w to apply".to_string(),
                        needs_refresh: false,
                        draft: Some(cfg),
                        scan: None,
                        uncovered: None,
                        index: None,
                    },
                    Err(e) => crate::tui::job::JobResult {
                        toast: format!("Claude draft failed: {e}"),
                        needs_refresh: false,
                        draft: None,
                        scan: None,
                        uncovered: None,
                        index: None,
                    },
                }
            },
        ));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        self.toast = None;
        self.toast_at_ms = None;
        // While a job is running, swallow all keys except Esc (detach) and 'q' (quit).
        if self.job.is_some() {
            match key.code {
                KeyCode::Esc => {
                    // Move the receiver into detached so the result is still
                    // delivered when the worker finishes. Generic: whatever
                    // Action was in the modal slot, no per-job view state
                    // depends on it living or dying detached.
                    if let Some(j) = self.job.take() {
                        self.detached.push(crate::tui::job::Detached {
                            kind: crate::tui::job::DetachedKind::Generic,
                            rx: j.rx,
                        });
                    }
                }
                KeyCode::Char('q') => {
                    self.should_quit = true;
                }
                _ => {}
            }
            return Ok(());
        }
        let n = self.tabs.len();
        if !self.tabs[self.active].claims_key(key.code) {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    return self.apply_action(Action::Quit);
                }
                KeyCode::Char('r') => {
                    return self.apply_action(Action::Refresh);
                }
                KeyCode::Char('R') => {
                    if crate::util::claude_on_path() {
                        return self.apply_action(Action::RelaunchClaude);
                    }
                    self.set_toast("`claude` not on PATH — cannot relaunch".to_string());
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.active = (self.active + 1) % n;
                    return Ok(());
                }
                KeyCode::BackTab => {
                    self.active = (self.active + n - 1) % n;
                    return Ok(());
                }
                _ => {}
            }
        }
        if let Some(action) = self.tabs[self.active].on_key(key, &self.ctx, &self.snap) {
            self.apply_action(action)?;
        }
        self.persist_active_config();
        Ok(())
    }

    pub fn render(&mut self, f: &mut Frame, now_ms: i64, now_local: OffsetDateTime) {
        // Auto-expire the toast after ~4000 ms of wall-clock time.
        if let (Some(_), Some(at)) = (&self.toast, self.toast_at_ms) {
            if now_ms - at > 4000 {
                self.toast = None;
                self.toast_at_ms = None;
            }
        }

        // Paint the warm background across the whole area.
        f.render_widget(Block::default().style(theme::bg()), f.area());

        // Left-aligned, width-capped panel region (the inline viewport already
        // bounds the height).
        let outer = Self::content_area(f.area());

        // A single rounded outer frame delineates the inline panel from the
        // surrounding terminal output; interior padding gives breathing room.
        // (Direction A keeps content borderless — this is the one outer frame.)
        let frame_block = Block::bordered()
            .border_type(theme::BORDER)
            .border_style(theme::accent_dim())
            .style(theme::bg())
            .padding(Padding::new(2, 2, 1, 0));
        let inner = frame_block.inner(outer);
        f.render_widget(frame_block, outer);

        let chunks = Layout::vertical([
            Constraint::Length(2), // header + rule
            Constraint::Length(2), // tab strip + accent rule
            Constraint::Length(1), // breathing gap
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(inner);

        // ---- header: app name (accent) ......... ● active   HH:MM:SS ----
        let active = self
            .snap
            .accounts
            .iter()
            .find(|a| a.is_active)
            .map(|a| a.alias.as_str());
        let left = Span::styled("cc-loadout", theme::accent());
        // Right-aligned group: ● active alias + live clock, flush to the
        // column's right edge.
        let mut right: Vec<Span> = Vec::new();
        if let Some(a) = active {
            right.push(Span::styled("● ", theme::pulse(self.frame)));
            right.push(Span::styled(format!("{a}  "), theme::text()));
        }
        right.push(Span::styled(
            format!(
                "{:02}:{:02}:{:02}",
                now_local.hour(),
                now_local.minute(),
                now_local.second()
            ),
            theme::dim(),
        ));
        let left_w = "cc-loadout".chars().count();
        let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
        let gap = (chunks[0].width as usize)
            .saturating_sub(left_w + right_w)
            .max(1);
        let mut header: Vec<Span> = vec![left, Span::raw(" ".repeat(gap))];
        header.extend(right);
        // Render header + faint rule as a single two-line Paragraph so that
        // ratatui clips to the chunk area at small terminal heights rather than
        // indexing past the buffer boundary (which would panic).
        let header_rule = "─".repeat(chunks[0].width as usize);
        f.render_widget(
            Paragraph::new(vec![
                Line::from(header),
                Line::from(Span::styled(header_rule, theme::faint())),
            ]),
            chunks[0],
        );

        // ---- tab strip: text, active in accent, with an accent underline rule ----
        let mut tabline: Vec<Span> = Vec::new();
        let mut rule = String::new();
        for (i, t) in self.tabs.iter().enumerate() {
            let title = t.title();
            if i == self.active {
                tabline.push(Span::styled(title.to_string(), theme::selected_tab()));
                rule.push_str(&"═".repeat(title.len()));
            } else {
                tabline.push(Span::styled(title.to_string(), theme::dim()));
                rule.push_str(&" ".repeat(title.len()));
            }
            tabline.push(Span::raw("   "));
            rule.push_str("   ");
        }
        // Same two-line approach for the tab strip + accent rule.
        f.render_widget(
            Paragraph::new(vec![
                Line::from(tabline),
                Line::from(Span::styled(rule, theme::accent())),
            ]),
            chunks[1],
        );

        // ---- body ----
        if let Some(job) = &self.job {
            self.render_spinner(f, chunks[3], job, now_ms);
        } else {
            self.tabs[self.active].render(f, chunks[3], &self.snap, now_ms, now_local);
        }

        // ---- footer: toast, else contextual hints ----
        let footer = if let Some(t) = &self.toast {
            Line::from(Span::styled(t.clone(), theme::accent_soft()))
        } else if self.job.is_some() {
            Line::from(Span::styled("esc dismiss · q quit", theme::dim()))
        } else {
            let mut spans: Vec<Span> = Vec::new();
            for (k, label) in self.tabs[self.active].footer_hints() {
                spans.push(Span::styled(format!("{k} "), theme::faint()));
                spans.push(Span::styled(format!("{label}   "), theme::dim()));
            }
            spans.push(Span::styled("⇥ tab   q quit", theme::dim()));
            Line::from(spans)
        };
        f.render_widget(Paragraph::new(footer), chunks[4]);
    }

    fn render_spinner(&self, f: &mut Frame, area: Rect, job: &crate::tui::job::Job, now_ms: i64) {
        use crate::tui::widgets::centered_rect;
        use ratatui::widgets::Clear;

        let panel_area = centered_rect(60, 20, area);
        f.render_widget(Clear, panel_area);

        let block = Block::bordered()
            .style(theme::panel())
            .border_type(theme::BORDER)
            .border_style(theme::accent_dim());
        let inner = block.inner(panel_area);
        f.render_widget(block, panel_area);

        let elapsed = ((now_ms - job.started_ms).max(0)) / 1000;
        let line = Line::from(vec![
            Span::styled(
                format!("{} ", theme::spinner_frame(self.frame)),
                theme::accent(),
            ),
            Span::styled(format!("{}… ", job.label), theme::text()),
            Span::styled(format!("{elapsed}s"), theme::dim()),
        ]);
        f.render_widget(Paragraph::new(line).style(theme::text()), inner);
    }

    /// Compute the content column within `full`: left-aligned, capped at a
    /// maximum width so the header line and prose do not stretch across very
    /// wide terminals. The hub runs in a compact inline viewport, so the height
    /// is already bounded and no vertical centering or margin is applied.
    fn content_area(full: Rect) -> Rect {
        const MAX_W: u16 = 96;
        Rect {
            x: full.x,
            y: full.y,
            width: full.width.min(MAX_W),
            height: full.height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::paths;
    use crate::account::store::Store;

    fn empty_ctx() -> (tempfile::TempDir, tempfile::TempDir, AppCtx) {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("nope.json"),
            registry_path: hdir.path().join("nope-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        (hdir, ddir, ctx)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    #[test]
    fn renders_header_and_accent_tabline() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        let mut t = Terminal::new(TestBackend::new(70, 12)).unwrap();
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        t.draw(|f| app.render(f, 1_000_000_000, now)).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("cc-loadout"));
        assert!(text.contains("Overview"));
        assert!(text.contains("⇥ tab"));
    }

    #[test]
    fn content_area_caps_width_left_aligned() {
        // Wide terminal: width capped at 96, left-aligned (no centering offset),
        // full height preserved (the inline viewport bounds the height).
        let a = App::content_area(Rect {
            x: 0,
            y: 0,
            width: 200,
            height: 50,
        });
        assert_eq!(a.width, 96);
        assert_eq!(a.x, 0, "left-aligned, not centered");
        assert_eq!(a.y, 0);
        assert_eq!(a.height, 50);
        // Narrow terminal: fills full width.
        let b = App::content_area(Rect {
            x: 0,
            y: 0,
            width: 70,
            height: 50,
        });
        assert_eq!(b.width, 70);
        assert_eq!(b.x, 0);
    }

    #[test]
    fn new_selects_initial_tab() {
        let ctx = empty_ctx().2;
        let app = App::new(ctx, 1).unwrap();
        assert_eq!(app.active_index(), 1);
    }

    #[test]
    fn tab_cycles_and_q_quits() {
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        assert_eq!(app.active_index(), 0);

        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.active_index(), 1);

        app.handle_key(key(KeyCode::BackTab)).unwrap();
        assert_eq!(app.active_index(), 0);

        app.handle_key(key(KeyCode::BackTab)).unwrap();
        assert_eq!(app.active_index(), 4, "wraps to last tab");

        // Return to Overview before testing quit.
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.active_index(), 0, "wraps back to first");

        assert!(!app.should_quit);
        app.handle_key(key(KeyCode::Char('q'))).unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn relaunch_action_sets_flag() {
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        assert!(!app.relaunch);
        app.apply_action(Action::RelaunchClaude).unwrap();
        assert!(app.relaunch);
    }

    #[test]
    fn apply_switch_changes_active_account() {
        use crate::account::{add, paths, store::Store};
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"w"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"w@x"}}"#,
        )
        .unwrap();
        let claude = paths::resolve(home, None);
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"p"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"p@x"}}"#,
        )
        .unwrap();
        add(&store, &claude, "personal", false, 2).unwrap();

        let ctx = AppCtx {
            store,
            claude,
            home: home.to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: home.join("none.json"),
            registry_path: home.join("nope-registry.json"),
            cwd: home.to_path_buf(),
        };
        let mut app = App::new(ctx, 0).unwrap();
        app.apply_action(Action::Switch("work".to_string()))
            .unwrap();
        // Switch is now async: drain the background job before asserting.
        let result = app.job.as_ref().unwrap().rx.recv().unwrap();
        assert!(result.toast.contains("switched to 'work'"));
    }

    #[test]
    fn apply_write_profiles_persists_json() {
        use crate::profile::config::{load, Profiles};
        let (_h, _d, ctx) = empty_ctx();
        let cfg_path = ctx.cfg_path.clone();
        let mut app = App::new(ctx, 0).unwrap();
        let profiles = Profiles {
            universal: vec!["u@m".to_string()],
            ..Profiles::default()
        };
        app.apply_action(Action::Commit {
            cfg: profiles,
            repos: vec![],
            expected: vec![],
        })
        .unwrap();
        // Commit is async: drain the background job to ensure the write
        // has completed before reading back from disk.
        let result = app.job.as_ref().unwrap().rx.recv().unwrap();
        assert!(
            result.toast.contains("wrote"),
            "expected toast about write, got: {}",
            result.toast
        );
        assert!(
            !result.toast.contains("matched differently"),
            "no repos written => no divergence suffix, got: {}",
            result.toast
        );
        let on_disk = load(&cfg_path).unwrap();
        assert_eq!(on_disk.universal, vec!["u@m".to_string()]);
    }

    #[test]
    fn commit_toast_appends_divergence_count_when_fresh_disagrees_with_preview() {
        use crate::profile::config::{Detect, Profile, Profiles};
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();

        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("Cargo.toml"), "[package]").unwrap();
        let repo = repo_dir.path().to_path_buf();

        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            "rust".to_string(),
            Profile {
                plugins: vec!["ra@x".to_string()],
                detect: Detect {
                    marker_files: vec!["Cargo.toml".to_string()],
                    ..Default::default()
                },
            },
        );
        let cfg = Profiles {
            profiles,
            ..Profiles::default()
        };

        // `expected` (the stale preview) claims no match, but the repo has
        // Cargo.toml on disk right now — fresh write-time detect disagrees.
        app.apply_action(Action::Commit {
            cfg,
            repos: vec![repo.clone()],
            expected: vec![(repo, vec![])],
        })
        .unwrap();
        let result = app.job.as_ref().unwrap().rx.recv().unwrap();
        assert!(
            result.toast.contains("1 matched differently than preview"),
            "expected divergence suffix, got: {}",
            result.toast
        );
    }

    #[test]
    fn profile_rule_edit_autosaves_without_apply() {
        // Regression: adding a rule in Detail must persist to profiles.json
        // immediately — no `w`/apply, no separate "done" needed.
        let (h, _d, mut app) = app_with_profile(3); // Profile tab; cfg has "rust"
        let cfg_path = h.path().join("profiles.json");

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // -> by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> rust
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // open builder (kind pick)
        app.handle_key(key(KeyCode::Enter)).unwrap(); // choose "path under" (kind 0)
        for c in "/workspace/".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit rule -> autosave

        let on_disk = crate::profile::config::load(&cfg_path).unwrap();
        assert_eq!(
            on_disk.profiles["rust"].detect.path_prefixes,
            vec!["/workspace/".to_string()],
            "rule edit must be on disk without pressing w"
        );
    }

    #[test]
    fn autosave_failure_surfaces_toast() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        // Unwritable cfg path: its parent is a regular file, so create_dir_all
        // (and thus the atomic write) always fails — even as root.
        let blocker = hdir.path().join("blk");
        std::fs::write(&blocker, b"x").unwrap();
        let cfg_path = blocker.join("profiles.json");
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path,
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab, ByPlugin default
                                                 // Mutate working via the scan-roots manager (needs no profile/plugins).
        app.handle_key(key(KeyCode::Char('r'))).unwrap(); // open roots manager
        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // add -> input seeded home/
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit input into the list
        app.handle_key(key(KeyCode::Esc)).unwrap(); // close -> commit roots -> autosave
        assert!(
            app.toast.as_deref().unwrap_or("").contains("save failed"),
            "autosave failure must surface a toast, got: {:?}",
            app.toast
        );
    }

    #[test]
    fn rescan_toast_appends_budget_suffix_only_when_nonzero() {
        assert_eq!(rescan_toast(3, 0), "scanned 3 repos");
        assert_eq!(
            rescan_toast(3, 2),
            "scanned 3 repos \u{b7} 2 repos hit walk budget"
        );
        assert_eq!(rescan_toast(0, 0), "scanned 0 repos");
    }

    #[test]
    fn rescan_indexes_glob_rule_atoms_and_leaves_the_rules_count_definite() {
        // A fresh scan indexes the FULL current-rule vocabulary (Task 9's
        // contract), so a committed glob rule's atom must land in rule_hits
        // at scan time, the saved cache must be stamped v2, and the Rules
        // tab's match count must be a definite number — never the pending
        // ellipsis — the very first time the profile's Detail is opened.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let work = hdir.path().join("work");
        let repo = work.join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("App.svelte"), "").unwrap();
        let root = work.display().to_string();

        let cfg_json = r#"{"scan_roots":["__ROOT__"],"universal":[],"profiles":{"frontend":{"plugins":[],"detect":{"marker_globs":["*.svelte"]}}}}"#
            .replace("__ROOT__", &root);
        std::fs::write(hdir.path().join("profiles.json"), cfg_json).unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab

        app.handle_key(key(KeyCode::Char('s'))).unwrap(); // dispatch Rescan on the job thread
        drain_until_idle(&mut app);

        let cache =
            crate::profile::scan_cache::load(ddir.path()).expect("scan must persist a cache");
        assert_eq!(
            cache.version,
            crate::profile::scan_cache::SCAN_CACHE_VERSION,
            "saved cache must be stamped with the current version"
        );
        let repo_cache = cache
            .repos
            .iter()
            .find(|r| r.path.ends_with("/svc"))
            .expect("svc repo must be scanned");
        assert_eq!(
            repo_cache.rule_hits.get("glob:*.svelte"),
            Some(&true),
            "the committed rule's glob atom must be indexed at scan time: {:?}",
            repo_cache.rule_hits
        );

        // Open the "frontend" profile's Rules tab: the match count must be
        // definite, never the pending ellipsis, since the scan just indexed
        // every atom the committed rule needs.
        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> "frontend"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        let text = render_profile(&mut app);
        assert!(
            text.contains("matches 1 of 1"),
            "Rules count must be a definite number right after Rescan: {text}"
        );
        assert!(
            !text.contains('\u{2026}'),
            "Rules count must never show the pending ellipsis after a fresh Rescan: {text}"
        );
    }

    #[test]
    fn rescan_writes_scan_cache() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        // A real sibling git repo under the scan root (parent of cwd).
        let work = hdir.path().join("work");
        let svc = work.join("svc");
        std::fs::create_dir_all(svc.join(".git")).unwrap();
        std::fs::write(svc.join("Cargo.toml"), "[package]").unwrap();
        let cwd = work.join("cli");
        std::fs::create_dir_all(&cwd).unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("none.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd,
        };
        let mut app = App::new(ctx, 3).unwrap();
        app.handle_key(key(KeyCode::Char('s'))).unwrap(); // scan on the job thread
        drain_until_idle(&mut app);

        let cache =
            crate::profile::scan_cache::load(ddir.path()).expect("scan must persist a cache");
        assert!(
            cache.repos.iter().any(|r| r.path.ends_with("/svc")),
            "cache must contain the scanned repo, got {:?}",
            cache.repos
        );
        assert!(cache.scanned_at > 0, "cache must record a scan time");
    }

    // ── Task 8: detached IndexAtoms job ───────────────────────────────────────

    #[test]
    fn index_atoms_dispatches_detached_job_keys_keep_working_and_cache_updates() {
        // Committing a rule whose atom the index has never seen must be
        // answered on a background thread — never through the modal `self.job`
        // slot, or the keyboard would freeze until the walk finishes.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let repo_dir = hdir.path().join("repo1");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join("App.tsx"), "").unwrap(); // makes the new atom answer true
        let repo_path = std::fs::canonicalize(&repo_dir)
            .unwrap()
            .display()
            .to_string();
        let root = hdir.path().display().to_string();

        let cfg_json = r#"{"scan_roots":["__ROOT__"],"universal":[],"profiles":{"web":{"plugins":[],"detect":{}}}}"#
            .replace("__ROOT__", &root);
        std::fs::write(hdir.path().join("profiles.json"), cfg_json).unwrap();
        crate::profile::scan_cache::save(
            ddir.path(),
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: vec![root.clone()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: repo_path.clone(),
                    marker_files: vec![],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: Default::default(), // nothing indexed yet
                    override_names: None,
                }],
                uncovered: Some(vec![repo_path.clone()]),
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab; cache seeds inv.repos

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> "web"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // builder (kind-pick)
        app.handle_key(key(KeyCode::Down)).unwrap();
        app.handle_key(key(KeyCode::Down)).unwrap(); // -> "has any"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // choose "has any"
        for c in "*.tsx".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit -> dispatches Action::IndexAtoms

        assert_eq!(
            app.detached.len(),
            1,
            "the index job must be detached, not modal"
        );
        assert!(
            app.job.is_none(),
            "the index job must never occupy the modal job slot"
        );

        // Keys keep working while the job runs.
        app.handle_key(key(KeyCode::Esc)).unwrap(); // leave Detail, keeping the rule
        assert_eq!(
            app.active_index(),
            3,
            "still on Profile after leaving Detail"
        );
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            app.active_index(),
            4,
            "Tab must still cycle top-level tabs while the index job runs"
        );
        app.handle_key(key(KeyCode::BackTab)).unwrap(); // back to Profile
        assert_eq!(app.active_index(), 3);

        drain_until_settled(&mut app);
        assert!(
            app.detached.is_empty(),
            "the job result must have been delivered"
        );

        let cache = crate::profile::scan_cache::load(ddir.path()).unwrap();
        let repo_cache = cache
            .repos
            .iter()
            .find(|r| r.path == repo_path)
            .expect("the repo must still be in the cache");
        assert_eq!(
            repo_cache.rule_hits.get("glob:*.tsx"),
            Some(&true),
            "the newly-answered atom must be persisted into the scan cache: {:?}",
            repo_cache.rule_hits
        );

        // Reopen Detail: the count line must now be definite (no pending cue).
        app.handle_key(key(KeyCode::Enter)).unwrap(); // reopen Detail on "web"
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        let text = render_profile(&mut app);
        assert!(
            text.contains("matches 1 of 1"),
            "count line must show a definite number once the atom is indexed: {text}"
        );
    }

    #[test]
    fn index_atoms_toast_names_atom_and_match_count_for_a_single_atom() {
        let (hdir, _ddir, ctx) = empty_ctx();
        let hit_repo = hdir.path().join("hit");
        let miss_repo = hdir.path().join("miss");
        std::fs::create_dir_all(&hit_repo).unwrap();
        std::fs::create_dir_all(&miss_repo).unwrap();
        std::fs::write(hit_repo.join("go.mod"), "").unwrap();

        let mut app = App::new(ctx, 0).unwrap();
        app.apply_action(Action::IndexAtoms {
            atoms: vec!["file:go.mod".to_string()],
            repos: vec![
                hit_repo.display().to_string(),
                miss_repo.display().to_string(),
            ],
        })
        .unwrap();
        assert_eq!(app.detached.len(), 1, "must dispatch detached, not modal");
        assert!(app.job.is_none());

        let result = app.detached[0].rx.recv().unwrap();
        assert_eq!(
            result.toast, "indexed file:go.mod \u{b7} 1 repos match",
            "singular toast names the atom and the exact match count"
        );
        let outcome = result.index.expect("IndexOutcome must be set");
        assert_eq!(outcome.atoms, vec!["file:go.mod".to_string()]);
        assert!(
            outcome.hits[&hit_repo.display().to_string()]["file:go.mod"],
            "the repo with go.mod must answer true"
        );
        assert!(
            !outcome.hits[&miss_repo.display().to_string()]["file:go.mod"],
            "the repo without go.mod must answer false"
        );
    }

    #[test]
    fn index_atoms_toast_reports_pattern_count_for_a_batch() {
        let (hdir, _ddir, ctx) = empty_ctx();
        let repo = hdir.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();

        let mut app = App::new(ctx, 0).unwrap();
        app.apply_action(Action::IndexAtoms {
            atoms: vec!["file:a".to_string(), "file:b".to_string()],
            repos: vec![repo.display().to_string()],
        })
        .unwrap();
        let result = app.detached[0].rx.recv().unwrap();
        assert_eq!(
            result.toast, "indexed 2 patterns",
            "a multi-atom batch reports the pattern count, not a per-atom match count"
        );
    }

    #[test]
    fn index_atoms_worker_death_clears_indexing_and_retries_the_batch() {
        // If the IndexAtoms job thread panics (or otherwise dies) before
        // sending a JobResult, its `tx` drops and the detached receiver
        // disconnects. `drain_jobs` must recover ProfileView.indexing (and
        // the open Detail's rules.indexing) — otherwise every future
        // IndexAtoms dispatch wedges forever behind the `!self.indexing`
        // guard, and the scan-bar/count-line "indexing …" text never clears.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let repo_dir = hdir.path().join("repo1");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo_path = std::fs::canonicalize(&repo_dir)
            .unwrap()
            .display()
            .to_string();
        let root = hdir.path().display().to_string();

        let cfg_json = r#"{"scan_roots":["__ROOT__"],"universal":[],"profiles":{"web":{"plugins":[],"detect":{}}}}"#
            .replace("__ROOT__", &root);
        std::fs::write(hdir.path().join("profiles.json"), cfg_json).unwrap();
        crate::profile::scan_cache::save(
            ddir.path(),
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: vec![root.clone()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: repo_path.clone(),
                    marker_files: vec![],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: Default::default(),
                    override_names: None,
                }],
                uncovered: Some(vec![repo_path.clone()]),
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> "web"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // builder (kind-pick)
        app.handle_key(key(KeyCode::Down)).unwrap();
        app.handle_key(key(KeyCode::Down)).unwrap(); // -> "has any"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // choose "has any"
        for c in "*.tsx".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit -> real IndexAtoms job dispatched

        assert_eq!(app.detached.len(), 1, "the real index job must be detached");
        assert!(app.job.is_none());

        // Simulate the worker dying before it could send a result: swap the
        // real job's receiver for one backed by a deliberately panicking
        // closure, tagged the same IndexAtoms kind a real dispatch would use.
        app.detached[0] = crate::tui::job::Detached {
            kind: crate::tui::job::DetachedKind::IndexAtoms,
            rx: crate::tui::job::spawn("simulated crash", 0, || panic!("simulated worker crash"))
                .rx,
        };

        drain_until_settled(&mut app);
        assert!(
            app.detached.is_empty(),
            "a disconnected receiver must not be kept forever"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("index"),
            "a user-visible signal must appear when the worker dies, got {:?}",
            app.toast
        );

        // The flag must not wedge: the very next key press re-dispatches the
        // SAME batch as a fresh job — proving `indexing` cleared and the
        // atom was requeued rather than silently dropped.
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            app.detached.len(),
            1,
            "indexing must not wedge: a new IndexAtoms batch must dispatch after the worker dies"
        );
    }

    fn render_profile(app: &mut App) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        let mut t = Terminal::new(TestBackend::new(90, 20)).unwrap();
        t.draw(|f| app.render(f, 1_000_000_000, now)).unwrap();
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn ctx_with_cache(hdir: &std::path::Path, ddir: &std::path::Path, cache_roots: &str) -> AppCtx {
        ctx_with_cache_uncovered(hdir, ddir, cache_roots, Some(vec![]))
    }

    fn ctx_with_cache_uncovered(
        hdir: &std::path::Path,
        ddir: &std::path::Path,
        cache_roots: &str,
        uncovered: Option<Vec<String>>,
    ) -> AppCtx {
        // profiles.json whose scan_roots are "/workspace".
        std::fs::write(
            hdir.join("profiles.json"),
            r#"{"scan_roots":["/workspace"],"universal":[],"profiles":{}}"#,
        )
        .unwrap();
        crate::profile::scan_cache::save(
            ddir,
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: vec![cache_roots.to_string()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: "/workspace/a".into(),
                    marker_files: vec!["Cargo.toml".into()],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: Default::default(),
                    override_names: None,
                }],
                uncovered,
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();
        AppCtx {
            store: Store::new(ddir),
            claude: paths::resolve(hdir, None),
            home: hdir.to_path_buf(),
            data_root: ddir.to_path_buf(),
            cfg_path: hdir.join("profiles.json"),
            registry_path: hdir.join("none-registry.json"),
            cwd: hdir.to_path_buf(),
        }
    }

    #[test]
    fn new_seeds_repos_from_scan_cache_when_roots_match() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_cache(hdir.path(), ddir.path(), "/workspace"); // matches
        let mut app = App::new(ctx, 3).unwrap();
        let text = render_profile(&mut app);
        assert!(
            text.contains("1 repos"),
            "cached repo count shown on reopen: {text}"
        );
    }

    /// Task 7: a profile rule whose atom was never indexed (absent from every
    /// scanned repo's `rule_hits`) must not block opening Detail — the count
    /// line reads "matches … of {total}" and prompts "press s to index"
    /// instead of guessing a number. Opening Detail must not spawn a
    /// background job either: the whole evaluation is signal-only.
    #[test]
    fn detail_rules_count_line_prompts_to_index_an_unindexed_atom() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        std::fs::write(
            hdir.path().join("profiles.json"),
            r#"{"scan_roots":["/workspace"],"universal":[],"profiles":{"rust":{"plugins":[],"detect":{"marker_globs":["*.tsx"]}}}}"#,
        )
        .unwrap();
        crate::profile::scan_cache::save(
            ddir.path(),
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: vec!["/workspace".to_string()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: "/workspace/does-not-exist-a".into(),
                    marker_files: vec![],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: Default::default(), // "glob:*.tsx" never indexed
                    override_names: None,
                }],
                uncovered: Some(vec![]),
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        open_rust_detail_rules(&mut app);
        assert!(
            app.job.is_none(),
            "opening Detail on an unindexed rule must not spawn a background \
             job — the whole evaluation is instant, zero-I/O"
        );
        let text = render_profile(&mut app);
        assert!(
            text.contains("matches \u{2026} of"),
            "an unindexed atom renders the pending ellipsis count: {text}"
        );
        assert!(
            text.contains("press s to index"),
            "an unindexed atom with no index job running prompts to index: {text}"
        );
    }

    /// Task 7: once every atom a profile's rules reference is present in the
    /// index, the count line reports a definite number tagged with the scan's
    /// age — no more "…", no more prompt.
    #[test]
    fn detail_rules_count_line_shows_a_definite_number_when_fully_indexed() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        std::fs::write(
            hdir.path().join("profiles.json"),
            r#"{"scan_roots":["/workspace"],"universal":[],"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        crate::profile::scan_cache::save(
            ddir.path(),
            &crate::profile::scan_cache::ScanCache {
                version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
                roots: vec!["/workspace".to_string()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: "/workspace/does-not-exist-a".into(),
                    marker_files: vec![],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: [("file:Cargo.toml".to_string(), true)]
                        .into_iter()
                        .collect(),
                    override_names: None,
                }],
                uncovered: Some(vec![]),
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        open_rust_detail_rules(&mut app);
        let text = render_profile(&mut app);
        assert!(
            text.contains("matches 1 of 1"),
            "a fully-indexed rule shows a definite count: {text}"
        );
        assert!(
            text.contains("as of"),
            "a fully-indexed count is tagged with the scan age: {text}"
        );
    }

    #[test]
    fn recompute_persists_uncovered_into_scan_cache() {
        // When the uncovered set changes in-session (here simulated via a
        // delivered background recompute), App must write it back to the scan
        // cache so the next startup seeds the fresh value without re-walking.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_cache_uncovered(hdir.path(), ddir.path(), "/workspace", Some(vec![]));
        let mut app = App::new(ctx, 3).unwrap();
        // Deliver a recompute result to the Profile tab, then let the event-loop
        // persistence hook run as it does after every key/job.
        app.tabs[crate::tui::TAB_PROFILE].accept_uncovered(vec!["/workspace/a".into()]);
        app.persist_active_config();
        let cache = crate::profile::scan_cache::load(ddir.path()).unwrap();
        assert_eq!(
            cache.uncovered,
            Some(vec!["/workspace/a".to_string()]),
            "a changed uncovered set must be persisted back to the scan cache"
        );
    }

    #[test]
    fn new_does_not_spawn_a_rebuild_for_a_current_version_cache_with_uncovered_none() {
        // Renamed from `new_does_not_spawn_a_backfill_job_for_a_legacy_cache`
        // (Task 9 era): `ctx_with_cache_uncovered` always stamps the cache
        // with `SCAN_CACHE_VERSION` (the helper has no `version` parameter),
        // so despite the old name this was never actually a v1/stale-version
        // cache — it is a CURRENT-version cache whose `uncovered` just
        // happens to be `None`. Task 10's rebuild is purely version-driven
        // (`cache.version < SCAN_CACHE_VERSION`), so this same-version cache
        // must still spawn NO job — only the Task 6 pending-seed applies.
        // The true v1-migration scenario (version < SCAN_CACHE_VERSION) is
        // covered by `v1_cache_with_uncovered_none_also_spawns_exactly_one_rebuild_job`
        // below, which uses an explicit `version: 0` cache.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        // None uncovered + one repo (/workspace/a, nonexistent) under empty profiles.
        let ctx = ctx_with_cache_uncovered(hdir.path(), ddir.path(), "/workspace", None);
        let mut app = App::new(ctx, 1).unwrap(); // active = Accounts, NOT Profile
        assert!(
            app.detached.is_empty(),
            "no rebuild job is scheduled for a same-version cache"
        );
        drain_until_settled(&mut app);
        assert_eq!(
            crate::profile::scan_cache::load(ddir.path())
                .unwrap()
                .uncovered,
            None,
            "the cache is left untouched — nothing rebuilds a same-version cache"
        );
    }

    #[test]
    fn new_does_not_backfill_when_uncovered_already_computed() {
        // A cache with uncovered: Some(_) (even empty) is already computed —
        // startup must NOT re-walk (no backfill job), so an idle drain leaves it
        // exactly as-is.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_cache_uncovered(hdir.path(), ddir.path(), "/workspace", Some(vec![]));
        let mut app = App::new(ctx, 1).unwrap();
        assert!(
            app.detached.is_empty(),
            "a computed cache must NOT schedule a backfill job"
        );
        drain_until_settled(&mut app);
        assert_eq!(
            crate::profile::scan_cache::load(ddir.path())
                .unwrap()
                .uncovered,
            Some(vec![]),
            "a computed-empty cache must stay empty (no re-walk on startup)"
        );
    }

    // ── Task 10: v1 scan-cache migration (banner + detached rebuild) ────────

    /// Build a context with a genuinely stale-version (`version: 0`) scan
    /// cache whose one repo (`{root}/svc`, a real `.git` directory with an
    /// `App.svelte` file) sits under `root` — so a real background rebuild
    /// walk has something concrete to find and index.
    fn ctx_with_v1_cache(
        version: u32,
        uncovered: Option<Vec<String>>,
    ) -> (tempfile::TempDir, tempfile::TempDir, AppCtx, String) {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let work = hdir.path().join("work");
        let repo = work.join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("App.svelte"), "").unwrap();
        let root = work.display().to_string();

        let cfg_json = r#"{"scan_roots":["__ROOT__"],"universal":[],"profiles":{"frontend":{"plugins":[],"detect":{"marker_globs":["*.svelte"]}}}}"#
            .replace("__ROOT__", &root);
        std::fs::write(hdir.path().join("profiles.json"), cfg_json).unwrap();

        crate::profile::scan_cache::save(
            ddir.path(),
            &crate::profile::scan_cache::ScanCache {
                version,
                roots: vec![root.clone()],
                repos: vec![crate::profile::discover::RepoSignal {
                    path: repo.display().to_string(),
                    marker_files: vec![],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                    languages: vec![],
                    rule_hits: Default::default(), // v1: nothing indexed yet
                    override_names: None,
                }],
                uncovered,
                scanned_at: 1_700_000_000,
            },
        )
        .unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        (hdir, ddir, ctx, root)
    }

    #[test]
    fn v1_cache_with_uncovered_some_spawns_a_detached_rebuild_and_the_banner_clears_once_it_lands()
    {
        // The typical July-era shape: version < SCAN_CACHE_VERSION, but
        // `uncovered` was already computed by the old (pre-index) code path.
        let (_hdir, ddir, ctx, _root) = ctx_with_v1_cache(0, Some(vec![]));
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab

        assert_eq!(
            app.detached.len(),
            1,
            "a stale-version cache with repos must spawn exactly one detached rebuild"
        );
        let text = render_profile(&mut app);
        assert!(
            text.contains("rebuilding in background"),
            "the banner must render while the rebuild is in flight: {text}"
        );

        drain_until_settled(&mut app);

        let text = render_profile(&mut app);
        assert!(
            !text.contains("rebuilding in background"),
            "the banner must clear once the rebuild lands: {text}"
        );

        let cache =
            crate::profile::scan_cache::load(ddir.path()).expect("rebuild must persist a cache");
        assert_eq!(
            cache.version,
            crate::profile::scan_cache::SCAN_CACHE_VERSION,
            "the rebuilt cache must be stamped with the current version"
        );
        let repo_cache = cache
            .repos
            .iter()
            .find(|r| r.path.ends_with("/svc"))
            .expect("svc repo must be reindexed by the rebuild");
        assert_eq!(
            repo_cache.rule_hits.get("glob:*.svelte"),
            Some(&true),
            "the rebuild must index rule_hits, not just bump the version: {:?}",
            repo_cache.rule_hits
        );
    }

    #[test]
    fn v1_cache_with_uncovered_none_also_spawns_exactly_one_rebuild_job() {
        // A v1 cache whose `uncovered` is None seeds Task 6's pending flag AND
        // needs Task 10's rebuild — both are true here, but there must be
        // exactly ONE detached job (no double-spawn between the two seeds).
        let (_hdir, ddir, ctx, _root) = ctx_with_v1_cache(0, None);
        let mut app = App::new(ctx, 3).unwrap();

        assert_eq!(
            app.detached.len(),
            1,
            "uncovered:None must not cause a second spawn alongside the rebuild"
        );
        let text = render_profile(&mut app);
        assert!(
            text.contains("rebuilding in background"),
            "the banner must render for this case too: {text}"
        );

        drain_until_settled(&mut app);

        let text = render_profile(&mut app);
        assert!(
            !text.contains("rebuilding in background"),
            "the banner must clear once the rebuild lands: {text}"
        );
        let cache =
            crate::profile::scan_cache::load(ddir.path()).expect("rebuild must persist a cache");
        assert_eq!(
            cache.version,
            crate::profile::scan_cache::SCAN_CACHE_VERSION,
            "the rebuilt cache must be stamped with the current version"
        );
    }

    #[test]
    fn rebuild_worker_death_clears_the_banner_and_leaves_no_wedged_detached_entry() {
        // Mirrors `index_atoms_worker_death_clears_indexing_and_retries_the_batch`:
        // a rebuild worker that dies before sending a ScanOutcome must not
        // leave the banner up forever, and drain_jobs must not keep the dead
        // receiver around.
        let (_hdir, _ddir, ctx, _root) = ctx_with_v1_cache(0, Some(vec![]));
        let mut app = App::new(ctx, 3).unwrap();
        assert_eq!(
            app.detached.len(),
            1,
            "the real rebuild job must be detached"
        );

        // Simulate the worker dying before it could send a result: swap the
        // real job's receiver for one backed by a deliberately panicking
        // closure, tagged the same Rebuild kind a real dispatch would use.
        app.detached[0] = crate::tui::job::Detached {
            kind: crate::tui::job::DetachedKind::Rebuild,
            rx: crate::tui::job::spawn("simulated crash", 0, || panic!("simulated worker crash"))
                .rx,
        };

        drain_until_settled(&mut app);
        assert!(
            app.detached.is_empty(),
            "a disconnected rebuild receiver must not be kept forever"
        );

        let text = render_profile(&mut app);
        assert!(
            !text.contains("rebuilding in background"),
            "a dead rebuild worker must clear the banner: {text}"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("rebuild"),
            "a user-visible signal must appear when the rebuild worker dies, got {:?}",
            app.toast
        );
    }

    #[test]
    fn new_ignores_scan_cache_on_roots_mismatch() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_cache(hdir.path(), ddir.path(), "/elsewhere"); // mismatch
        let mut app = App::new(ctx, 3).unwrap();
        let text = render_profile(&mut app);
        assert!(
            !text.contains("1 repos"),
            "stale-root cache must be ignored: {text}"
        );
    }

    #[test]
    fn write_profiles_also_applies_global() {
        use crate::profile::config::Profiles;
        let (_h, _d, ctx) = empty_ctx();
        let settings = crate::profile::apply::global_settings_path(&ctx.claude);
        let mut app = App::new(ctx, 0).unwrap();
        let profiles = Profiles {
            universal: vec!["u@m".to_string()],
            profiles: [(
                "rust".to_string(),
                crate::profile::config::Profile {
                    plugins: vec!["r@m".to_string()],
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
            ..Profiles::default()
        };
        app.apply_action(Action::Commit {
            cfg: profiles,
            repos: vec![],
            expected: vec![],
        })
        .unwrap();
        // Drain the background job so the writes complete.
        let _ = app.job.as_ref().unwrap().rx.recv().unwrap();
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(on_disk["enabledPlugins"]["u@m"], serde_json::json!(true));
        assert_eq!(on_disk["enabledPlugins"]["r@m"], serde_json::json!(false));
    }

    #[test]
    fn tick_increments_frame() {
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        assert_eq!(app.frame, 0);
        app.tick();
        assert_eq!(app.frame, 1);
        app.tick();
        assert_eq!(app.frame, 2);
    }

    #[test]
    fn animating_true_when_toast_set() {
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        assert!(!app.animating(), "no toast, no job → idle");
        app.set_toast("hello".to_string());
        assert!(app.animating(), "toast set → animating");
        app.toast = None;
        assert!(!app.animating(), "toast cleared → idle");
    }

    #[test]
    fn animating_true_when_job_set() {
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        assert!(!app.animating());
        // Use job::spawn so the Job is properly constructed (it now carries an rx).
        app.job = Some(crate::tui::job::spawn("test", 0, || {
            crate::tui::job::JobResult {
                toast: "done".into(),
                needs_refresh: false,
                draft: None,
                scan: None,
                uncovered: None,
                index: None,
            }
        }));
        assert!(app.animating(), "job present → animating");
        app.job = None;
        assert!(!app.animating());
    }

    #[test]
    fn app_new_falls_back_on_malformed_profiles_json() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let cfg_path = hdir.path().join("profiles.json");
        std::fs::write(&cfg_path, b"{ not json").unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path,
            registry_path: hdir.path().join("nope-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        // A corrupt profiles.json must not propagate an error — App::new should
        // degrade gracefully to a scan draft so all four tabs still open.
        assert!(
            App::new(ctx, 0).is_ok(),
            "App::new must succeed even when profiles.json is malformed"
        );
    }

    #[test]
    fn profile_board_does_not_capture_raw_input() {
        // The new Profile board tab (index 3) is a navigable list, not a text
        // entry wizard — claims_key('q') must return false, so q quits normally.
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 3).unwrap();
        // Profile tab is active; 'q' should trigger quit (board doesn't claim 'q').
        app.handle_key(KeyEvent::from(KeyCode::Char('q'))).unwrap();
        assert!(
            app.should_quit,
            "'q' on the board must quit (board does not claim 'q')"
        );
    }

    #[test]
    fn draft_action_with_no_claude_toasts_and_does_not_spawn() {
        // Verify the no-claude guard: when spawn_draft_job is called with
        // claude_bin = None it must set a toast and leave app.job as None.
        // (We inject None directly to avoid any PATH manipulation — hermetic
        //  and safe under #![deny(unsafe_code)].)
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();

        // Build a minimal Inventory (no plugins, no repos, no suggestions).
        let inv = crate::profile::discover::Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // Call the factored helper with None to simulate "claude not on PATH".
        app.spawn_draft_job(inv, vec![], None);

        assert!(
            app.job.is_none(),
            "no job must be spawned when claude_bin is None"
        );
        assert!(
            app.toast
                .as_deref()
                .unwrap_or("")
                .contains("claude not found"),
            "toast must mention 'claude not found', got: {:?}",
            app.toast
        );
    }

    #[test]
    fn new_defers_repo_scan_until_explicit_s() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();

        // Registry with one installed plugin.
        let plugins_dir = hdir.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();
        let reg = plugins_dir.join("installed_plugins.json");
        std::fs::write(
            &reg,
            r#"{"plugins":{"serena@official":[{"scope":"user"}]}}"#,
        )
        .unwrap();

        // A sibling Rust repo under the scan root (parent of cwd).
        let work = hdir.path().join("work");
        let svc = work.join("svc");
        std::fs::create_dir_all(svc.join(".git")).unwrap();
        std::fs::write(svc.join("Cargo.toml"), "[package]").unwrap();
        let cwd = work.join("cli");
        std::fs::create_dir_all(&cwd).unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("none.json"),
            registry_path: reg,
            cwd,
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab

        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        let render = |app: &mut App| -> String {
            let mut t = Terminal::new(TestBackend::new(90, 20)).unwrap();
            t.draw(|f| app.render(f, 1_000_000_000, now)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        // Default by-plugin view lists the plugin; no eager scan happened.
        let text = render(&mut app);
        assert!(
            text.contains("serena@official"),
            "lists the installed plugin"
        );
        assert!(text.contains("All plugins"), "shows the by-plugin header");

        // Toggle to by-profile: with no scan there is NO 'rust' bucket /
        // Cargo.toml detect (proves the scan was deferred, not run at startup).
        app.handle_key(key(KeyCode::Char('v'))).unwrap();
        let by_profile = render(&mut app);
        assert!(
            !by_profile.contains("Cargo.toml"),
            "deferred scan: no eager profile bucket should appear, got:\n{by_profile}"
        );

        // Explicit 's' scan now runs on the job thread; wait for it to land.
        app.handle_key(key(KeyCode::Char('s'))).unwrap();
        drain_until_idle(&mut app);
        let after_scan = render(&mut app);
        assert!(
            after_scan.contains("Cargo.toml"),
            "after 's', the rust bucket appears, got:\n{after_scan}"
        );
    }

    // ── Fix #1: render must not panic at small terminal heights ─────────────

    #[test]
    fn render_does_not_panic_at_small_heights() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        for h in [1u16, 2, 3, 4] {
            let (_h, _d, ctx) = empty_ctx();
            let mut app = App::new(ctx, 0).unwrap();
            let mut t = Terminal::new(TestBackend::new(70, h)).unwrap();
            // Must not panic regardless of how small the terminal is.
            t.draw(|f| app.render(f, 1_000_000_000, now)).unwrap();
        }
    }

    // ── Fix #2/#3: drain_jobs delivers results and clears state ─────────────

    #[test]
    fn drain_jobs_disconnected_clears_job() {
        use crate::tui::job::{Job, JobResult};
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        // Construct a Job whose sender has already been dropped so try_recv
        // returns Disconnected deterministically (no sleep needed).
        let (tx, rx) = std::sync::mpsc::channel::<JobResult>();
        drop(tx);
        app.job = Some(Job {
            label: "x".into(),
            started_ms: 0,
            rx,
        });
        app.drain_jobs(i64::MAX / 2).unwrap();
        assert!(app.job.is_none(), "job must be cleared on Disconnected");
        assert_eq!(
            app.toast.as_deref(),
            Some("operation aborted"),
            "toast must be set to 'operation aborted'"
        );
    }

    #[test]
    fn spinner_is_held_for_the_minimum_duration() {
        use crate::tui::job::JobResult;
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        let started = 10_000_000i64;
        app.job = Some(crate::tui::job::spawn("x", started, || JobResult {
            toast: "done".into(),
            needs_refresh: false,
            draft: None,
            scan: None,
            uncovered: None,
            index: None,
        }));
        // Let the trivial worker finish so its result is waiting in the channel.
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Within the floor: the spinner must persist and the result must NOT be
        // applied yet — even though the worker already finished (no flash).
        app.drain_jobs(started + 100).unwrap();
        assert!(
            app.job.is_some(),
            "spinner must persist within MIN_SPINNER_MS"
        );
        assert!(app.toast.is_none(), "result must not land within the floor");
        // Past the floor: the result lands and the job clears.
        app.drain_jobs(started + MIN_SPINNER_MS + 500).unwrap();
        assert!(app.job.is_none(), "job clears once the floor elapses");
        assert_eq!(app.toast.as_deref(), Some("done"));
    }

    #[test]
    fn r_reaches_detail_rename_via_routing() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        // A profiles.json with one profile so the by-profile board has a row to open.
        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            r#"{"universal":[],"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg,
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab

        // ByPlugin default → toggle to by-profile, move to the 'rust' row, open Detail.
        app.handle_key(key(KeyCode::Char('v'))).unwrap();
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal(0) -> rust(1)
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail(rust)
        assert!(
            !app.active_claims_key(KeyCode::Char('x')),
            "Detail normal does not claim 'x'"
        );

        // 'r' must reach the view (NOT be swallowed as global Refresh) and enter rename.
        app.handle_key(key(KeyCode::Char('r'))).unwrap();
        assert!(
            app.active_claims_key(KeyCode::Char('x')),
            "Detail rename (text entry) claims 'x'"
        );
    }

    /// Build an App on the Profile tab with one profile so Detail/Apply are reachable.
    fn app_with_profile(initial_tab: usize) -> (tempfile::TempDir, tempfile::TempDir, App) {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            r#"{"universal":[],"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg,
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let app = App::new(ctx, initial_tab).unwrap();
        (hdir, ddir, app)
    }

    #[test]
    fn tab_in_detail_does_not_switch_top_tab() {
        let (_h, _d, mut app) = app_with_profile(3); // Profile
        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // → 'rust' row
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        assert_eq!(app.active_index(), 3, "in Detail before Tab");
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            app.active_index(),
            3,
            "Tab in Detail must NOT switch the top-level tab (it toggles plugins/repos)"
        );
    }

    #[test]
    fn esc_in_apply_goes_back_not_quit() {
        let (_h, _d, mut app) = app_with_profile(3);
        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        app.handle_key(key(KeyCode::Esc)).unwrap();
        assert!(!app.should_quit, "Esc in Apply must NOT quit the app");
    }

    #[test]
    fn arrows_no_longer_switch_top_tabs() {
        let (_h, _d, mut app) = app_with_profile(0); // Overview
        app.handle_key(key(KeyCode::Right)).unwrap();
        assert_eq!(app.active_index(), 0, "→ must not switch tabs");
        app.handle_key(key(KeyCode::Left)).unwrap();
        assert_eq!(app.active_index(), 0, "← must not switch tabs");
    }

    #[test]
    fn tab_still_cycles_tabs_at_root() {
        let (_h, _d, mut app) = app_with_profile(0); // Overview (claims nothing)
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(app.active_index(), 1, "Tab cycles tabs when unclaimed");
    }

    #[test]
    fn esc_at_root_still_quits() {
        let (_h, _d, mut app) = app_with_profile(0);
        app.handle_key(key(KeyCode::Esc)).unwrap();
        assert!(app.should_quit, "Esc at a tab root (unclaimed) still quits");
    }

    #[test]
    fn esc_detaches_job_and_result_is_delivered() {
        use crate::tui::job::JobResult;
        let (_h, _d, ctx) = empty_ctx();
        let mut app = App::new(ctx, 0).unwrap();
        // Spawn a job that immediately returns a known toast.
        app.job = Some(crate::tui::job::spawn("bg-work", 0, || JobResult {
            toast: "bg-done".into(),
            needs_refresh: false,
            draft: None,
            scan: None,
            uncovered: None,
            index: None,
        }));
        assert!(app.job.is_some());
        // Pressing Esc moves the job to detached.
        app.handle_key(KeyEvent::from(KeyCode::Esc)).unwrap();
        assert!(app.job.is_none(), "job must be None after Esc");
        assert_eq!(app.detached.len(), 1, "receiver must be in detached");
        // The worker thread is fast; poll drain_jobs until the toast lands.
        let mut delivered = false;
        for _ in 0..50 {
            app.drain_jobs(i64::MAX / 2).unwrap();
            if app.toast.as_deref() == Some("bg-done") {
                delivered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            delivered,
            "detached job result must eventually be delivered as toast"
        );
        assert!(
            app.detached.is_empty(),
            "detached must be empty after delivery"
        );
    }

    // ── Task C: Profile → Detail → Rules keyboard path, driven through the
    //    REAL App::handle_key (claims_key → global-shortcut gate → on_key).
    //    These exercise the bugs the earlier `view.on_key(...)` tests could not
    //    reach, because they bypassed app.rs/claims_key entirely. ────────────

    /// Build an App on the Profile tab with one profile, returning the cfg path
    /// too so a committed config can be read back from disk.
    fn app_with_profile_and_cfg(
        initial_tab: usize,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        App,
        std::path::PathBuf,
    ) {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            r#"{"universal":[],"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg.clone(),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let app = App::new(ctx, initial_tab).unwrap();
        (hdir, ddir, app, cfg)
    }

    /// Open Detail on the 'rust' row and switch focus to the Rules tab.
    /// (ByPlugin default → 'v' to by-profile → Down to rust → Enter → Tab.)
    fn open_rust_detail_rules(app: &mut App) {
        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal(0) → rust(1)
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail(rust)
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus → Rules
    }

    /// Drain the (async) commit job spawned by Apply's Enter, blocking until the
    /// write completes, so the on-disk profiles.json can be read back.
    fn drain_commit(app: &App) {
        let _ = app.job.as_ref().unwrap().rx.recv().unwrap();
    }

    /// Wait for the active background job (e.g. a Rescan) to finish AND apply its
    /// result to the active view via `drain_jobs`, so `app.job` clears and
    /// subsequent keys are no longer swallowed by the job-in-flight guard.
    fn drain_until_idle(app: &mut App) {
        for _ in 0..400 {
            app.drain_jobs(i64::MAX / 2).unwrap();
            if app.job.is_none() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("background job did not finish in time");
    }

    /// Like `drain_until_idle` but also waits for detached jobs (e.g. one moved
    /// there via Esc) to land.
    fn drain_until_settled(app: &mut App) {
        for _ in 0..400 {
            app.drain_jobs(i64::MAX / 2).unwrap();
            if app.job.is_none() && app.detached.is_empty() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("background work did not finish in time");
    }

    /// (a) Critical: the `contains` builder must be reachable end-to-end through
    /// the real App path. The earlier regression test drove `view.on_key(...)`
    /// directly, so it never exercised `claims_key`/`App::handle_key`. Through
    /// the real path, Tab in contains value-entry was previously consumed as a
    /// TAB-CYCLE at the app level (claims_key returned false), so the word field
    /// was never reachable and `commit_editor` silently dropped the empty-word
    /// rule.
    ///
    /// RED before fix: the committed rule's `content` is empty (Tab cycled the
    /// top-level tab off Profile, so the keystrokes after it never reached the
    /// builder). GREEN after fix: `content == [{requirements.txt → torch}]`.
    #[test]
    fn contains_builder_tab_reaches_word_field_via_app() {
        let (_h, _d, mut app, cfg) = app_with_profile_and_cfg(3);
        open_rust_detail_rules(&mut app);

        // Stay on the Profile tab the whole time.
        assert_eq!(app.active_index(), 3, "in Detail/Rules before building");

        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // open builder (kind-pick)
                                                          // navigate to "contains" (index 3): three Downs
        app.handle_key(key(KeyCode::Down)).unwrap();
        app.handle_key(key(KeyCode::Down)).unwrap();
        app.handle_key(key(KeyCode::Down)).unwrap();
        app.handle_key(key(KeyCode::Enter)).unwrap(); // choose contains → file input
        for c in "requirements.txt".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        // The crux: Tab must reach the builder (switch file→word), NOT cycle tabs.
        app.handle_key(key(KeyCode::Tab)).unwrap();
        assert_eq!(
            app.active_index(),
            3,
            "Tab inside the contains builder must NOT cycle the top-level tab"
        );
        for c in "torch".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit rule
        app.handle_key(key(KeyCode::Enter)).unwrap(); // done → write back to working
        drain_until_idle(&mut app); // no-op now: the recompute is inline, not a job

        // Commit to disk via Apply, then read the profile back.
        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit
        drain_commit(&app);

        let on_disk = crate::profile::config::load(&cfg).unwrap();
        assert_eq!(
            on_disk.profiles["rust"].detect.content,
            vec![crate::profile::config::ContentRule {
                file: "requirements.txt".into(),
                word: "torch".into(),
            }],
            "contains rule must carry BOTH file and word (proves Tab reached the builder)"
        );
    }

    /// (b) Important: `q` during the kind-pick step must NOT quit the app. Before
    /// the fix, `editing_text()` is false during kind-pick, so claims_key fell to
    /// `Sub::Detail(_) => matches!(Esc|Tab|Char('r'))` — 'q' was unclaimed and
    /// `App::handle_key` ran `Action::Quit`.
    ///
    /// RED before fix: `app.should_quit == true`. GREEN after fix: the builder
    /// claims all keys while open, so 'q' is a literal/no-op and the app stays up.
    #[test]
    fn q_during_kind_pick_does_not_quit_via_app() {
        let (_h, _d, mut app, _cfg) = app_with_profile_and_cfg(3);
        open_rust_detail_rules(&mut app);

        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // open builder (kind-pick step)
        assert!(
            app.active_claims_key(KeyCode::Char('q')),
            "builder open: claims_key must claim 'q'"
        );
        app.handle_key(key(KeyCode::Char('q'))).unwrap();
        assert!(
            !app.should_quit,
            "'q' during kind-pick must NOT quit the app"
        );
        assert_eq!(app.active_index(), 3, "still on the Profile tab");
    }

    #[test]
    fn f_prefills_rules_from_a_scanned_repo_via_app() {
        // A temp repo with Cargo.toml that the Profile view can scan.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let scan_root = tempfile::tempdir().unwrap();
        let repo = scan_root.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            format!(
                r#"{{"scan_roots":["{}"],"universal":[],"profiles":{{"rust":{{"plugins":[],"detect":{{}}}}}}}}"#,
                scan_root.path().display()
            ),
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg.clone(),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Char('s'))).unwrap(); // scan -> inv.repos has the svc repo
        drain_until_idle(&mut app); // scan runs on the job thread; wait for it
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> rust
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail(rust)
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        app.handle_key(key(KeyCode::Char('f'))).unwrap(); // open repo picker
        app.handle_key(key(KeyCode::Enter)).unwrap(); // prefill from first repo
        app.handle_key(key(KeyCode::Enter)).unwrap(); // done -> write back
        drain_until_idle(&mut app); // no-op now: the recompute is inline, not a job

        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit
        drain_commit(&app);

        let on_disk = crate::profile::config::load(&cfg).unwrap();
        assert!(
            on_disk.profiles["rust"]
                .detect
                .marker_files
                .contains(&"Cargo.toml".to_string()),
            "f must prefill a has-file rule from the scanned repo; got {:?}",
            on_disk.profiles["rust"].detect
        );
    }

    /// (c) Important: `Delete` in the Rules tab must NOT remove the whole profile.
    /// Before the fix, detail.rs matched the `KeyCode::Delete` arm (inside the
    /// Rules branch) which did `working.profiles.remove(&self.name)`.
    ///
    /// RED before fix: after Delete + commit, profiles.json has no "rust".
    /// GREEN after fix: Delete deletes the selected RULE (routed to
    /// rules.handle_key) and the profile survives.
    #[test]
    fn delete_in_rules_tab_does_not_remove_profile_via_app() {
        let (_h, _d, mut app, cfg) = app_with_profile_and_cfg(3);
        open_rust_detail_rules(&mut app);

        // One rule exists (marker_files: ["Cargo.toml"]) and is selected (cursor 0).
        // Delete must remove that rule, not the profile.
        app.handle_key(key(KeyCode::Delete)).unwrap();
        assert_eq!(
            app.active_index(),
            3,
            "Delete on a rule must stay in Detail, not bounce to Board"
        );
        // Done → write back, then commit to disk and read profile back.
        app.handle_key(key(KeyCode::Enter)).unwrap(); // done from Rules → write back
        drain_until_idle(&mut app); // no-op now: the recompute is inline, not a job
        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit
        drain_commit(&app);

        let on_disk = crate::profile::config::load(&cfg).unwrap();
        assert!(
            on_disk.profiles.contains_key("rust"),
            "Delete in Rules tab must NOT remove the profile; profiles={:?}",
            on_disk.profiles.keys().collect::<Vec<_>>()
        );
        assert!(
            on_disk.profiles["rust"].detect.marker_files.is_empty(),
            "Delete should have removed the selected rule (Cargo.toml marker)"
        );
    }

    /// Regression test for the Critical keyboard-routing bug: Tab while the
    /// `f` (from repo) picker is open must NOT switch Detail focus from Rules
    /// to Plugins.
    ///
    /// Before the fix the Tab guard in `DetailState::handle_key` reads:
    ///   `focus == Plugins || editor.is_none()`
    /// The picker sets `repo_pick` but leaves `editor == None`, so the guard
    /// fires and Tab toggles the focus to Plugins.  Subsequent Enter then hits
    /// the "done from Plugins" path, writing back an EMPTY detect (the
    /// in-flight prefill is discarded), so the derived `Cargo.toml` marker is
    /// absent on disk.
    ///
    /// RED before fix: `marker_files` is empty — Tab stole focus, Enter went
    /// to the wrong path, and the prefill was lost.
    /// GREEN after fix (guard adds `&& !self.rules.is_picking()`): Tab is not
    /// consumed by the outer layer, the picker stays open, Enter applies the
    /// prefill, and the marker file persists on disk.
    #[test]
    fn tab_during_repo_picker_keeps_picker_focus_via_app() {
        // A temp repo with Cargo.toml that the Profile view can scan.
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let scan_root = tempfile::tempdir().unwrap();
        let repo = scan_root.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            format!(
                r#"{{"scan_roots":["{}"],"universal":[],"profiles":{{"rust":{{"plugins":[],"detect":{{}}}}}}}}"#,
                scan_root.path().display()
            ),
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg.clone(),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Char('s'))).unwrap(); // scan -> inv.repos has the svc repo
        drain_until_idle(&mut app); // scan runs on the job thread; wait for it
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> rust
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail(rust)
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules (no picker open; toggle OK)
        app.handle_key(key(KeyCode::Char('f'))).unwrap(); // open repo picker
                                                          // The crux: Tab must NOT switch focus away from the Rules/picker.
        app.handle_key(key(KeyCode::Tab)).unwrap();
        // If Tab stole focus, Enter here would hit "done from Plugins" and
        // write back empty detect — the rule would be absent.
        // If the picker still owns focus, Enter prefills from the repo.
        app.handle_key(key(KeyCode::Enter)).unwrap(); // prefill from first repo
        app.handle_key(key(KeyCode::Enter)).unwrap(); // done -> write back
        drain_until_idle(&mut app); // no-op now: the recompute is inline, not a job

        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit
        drain_commit(&app);

        let on_disk = crate::profile::config::load(&cfg).unwrap();
        assert!(
            on_disk.profiles["rust"]
                .detect
                .marker_files
                .contains(&"Cargo.toml".to_string()),
            "Tab during repo picker must not steal focus — derived Cargo.toml rule must persist; got {:?}",
            on_disk.profiles["rust"].detect
        );
    }

    // ── Task 3: TasksView tab registration + RunTask/RemoveTask handlers ────────

    /// Build an App whose data_root contains a pre-seeded tasks.json so that the
    /// snapshot has at least one task row. Returns the TempDirs (to keep them
    /// alive), a seeded task id, and the App.
    fn app_with_task(initial_tab: usize) -> (tempfile::TempDir, tempfile::TempDir, String, App) {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();

        // Write a minimal tasks.json with one real task.
        let tasks_json = serde_json::json!({
            "version": 1,
            "tasks": {
                "test-task": {
                    "kind": "task",
                    "account": "work",
                    "times": ["07:00"]
                }
            }
        });
        std::fs::write(
            ddir.path().join("tasks.json"),
            serde_json::to_vec_pretty(&tasks_json).unwrap(),
        )
        .unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: crate::account::paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("none.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let app = App::new(ctx, initial_tab).unwrap();
        (hdir, ddir, "test-task".to_string(), app)
    }

    /// Switching to TAB_TASKS (index 4) works and the tab is correctly registered.
    /// This is the RED test that fails before the tab is registered.
    #[test]
    fn tab_cycles_to_tasks_tab_at_index_4() {
        let (_h, _d, mut app) = app_with_profile(0); // Overview (tab 0)
                                                     // Tab forward 4 times to reach Tasks (index 4).
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Tab)).unwrap();
        }
        assert_eq!(
            app.active_index(),
            crate::tui::TAB_TASKS,
            "Tasks tab must be at index {}",
            crate::tui::TAB_TASKS
        );
    }

    /// Pressing `d` then `y` on the Tasks tab (with a real task in the snapshot)
    /// spawns a RemoveTask job via App::handle_key (not view.on_key directly).
    /// RED before Task 3: either the tab is not registered or the stub handler
    /// doesn't spawn a job. GREEN after: job.is_some().
    #[test]
    fn tasks_tab_d_y_spawns_remove_job_via_app() {
        let (_h, _d, _id, mut app) = app_with_task(crate::tui::TAB_TASKS);
        // 'd' arms the confirm-delete prompt; 'y' emits RemoveTask(id).
        // Both keys are claimed by TasksView so they are NOT consumed by app-level shortcuts.
        app.handle_key(key(KeyCode::Char('d'))).unwrap();
        app.handle_key(key(KeyCode::Char('y'))).unwrap();
        assert!(
            app.job.is_some(),
            "RemoveTask('test-task') must spawn a background job via App::handle_key"
        );
    }

    #[test]
    fn explain_opens_and_esc_closes_without_quitting_via_app() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let scan_root = tempfile::tempdir().unwrap();
        let repo = scan_root.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let cfg = hdir.path().join("profiles.json");
        std::fs::write(
            &cfg,
            format!(
                r#"{{"scan_roots":["{}"],"universal":[],"profiles":{{"rust":{{"plugins":[],"detect":{{"marker_files":["Cargo.toml"]}}}}}}}}"#,
                scan_root.path().display()
            ),
        )
        .unwrap();
        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: cfg,
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap();

        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Char('s'))).unwrap(); // scan
        drain_until_idle(&mut app); // scan runs on the job thread; wait for it
        app.handle_key(key(KeyCode::Down)).unwrap(); // -> rust
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // -> Rules
        app.handle_key(key(KeyCode::Char('?'))).unwrap(); // open explain

        // Esc must now be claimed (close overlay), not quit the app.
        assert!(
            app.active_claims_key(KeyCode::Esc),
            "explain overlay must claim Esc"
        );
        app.handle_key(key(KeyCode::Esc)).unwrap(); // close overlay
        assert!(
            !app.should_quit,
            "Esc in explain overlay must NOT quit the app"
        );
        assert_eq!(
            app.active_index(),
            3,
            "still on the Profile tab after closing explain"
        );
    }

    // ── Task 13: zero-I/O regression net ────────────────────────────────────

    /// End-to-end proof that once a repo's signals are indexed, every
    /// interactive path touching counts/drift/Apply reads the index — never
    /// the filesystem. The repo directory is deleted entirely right after the
    /// real scan that seeds the cache; a regression back to a disk walk on
    /// any of these paths would either see "not found" (flipping a definite
    /// match to a definite non-match, changing the rendered counts) or
    /// hang/slow-walk a nonexistent tree — the 1s bound below catches the
    /// latter even on the rare path where the former doesn't trip.
    #[test]
    fn zero_walk_after_repo_deletion_keeps_detail_edit_and_apply_definite() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let work = hdir.path().join("work");
        let repo = work.join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("App.vue"), "").unwrap();
        let root = work.display().to_string();

        let cfg_json = r#"{"scan_roots":["__ROOT__"],"universal":[],"profiles":{"frontend":{"plugins":[],"detect":{"marker_globs":["*.vue"]}}}}"#
            .replace("__ROOT__", &root);
        std::fs::write(hdir.path().join("profiles.json"), cfg_json).unwrap();

        let ctx = AppCtx {
            store: Store::new(ddir.path()),
            claude: paths::resolve(hdir.path(), None),
            home: hdir.path().to_path_buf(),
            data_root: ddir.path().to_path_buf(),
            cfg_path: hdir.path().join("profiles.json"),
            registry_path: hdir.path().join("none-registry.json"),
            cwd: hdir.path().to_path_buf(),
        };
        let mut app = App::new(ctx, 3).unwrap(); // Profile tab

        // Real scan while the repo still exists — the ONLY filesystem walk
        // in this whole test.
        app.handle_key(key(KeyCode::Char('s'))).unwrap();
        drain_until_idle(&mut app);

        // Delete the repo entirely. Everything from here on must answer from
        // the already-indexed signal, not the (now-absent) directory.
        std::fs::remove_dir_all(&repo).unwrap();
        assert!(!repo.exists());

        // ── Interaction 1: open Detail on the glob-rule profile ────────────
        let t0 = std::time::Instant::now();
        app.handle_key(key(KeyCode::Char('v'))).unwrap(); // by-profile board
        app.handle_key(key(KeyCode::Down)).unwrap(); // Universal -> "frontend"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // open Detail
        app.handle_key(key(KeyCode::Tab)).unwrap(); // focus -> Rules
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(1),
            "opening Detail must not block on a disk walk"
        );
        let text = render_profile(&mut app);
        assert!(
            text.contains("matches 1 of 1"),
            "count must stay definite even though the repo directory is gone: {text}"
        );
        assert!(
            !text.contains('\u{2026}'),
            "an already-indexed atom must never fall back to the pending ellipsis: {text}"
        );

        // ── Interaction 2: edit a rule — add a "has file" rule for
        // Cargo.toml. That atom is already in every repo's rule_hits (Task 9:
        // MARKER_FILES is indexed unconditionally at scan time, regardless of
        // which rules were live then), so this must resolve — and drift must
        // recompute — without dispatching a background IndexAtoms job. ───────
        let t1 = std::time::Instant::now();
        app.handle_key(key(KeyCode::Char('a'))).unwrap(); // open builder (kind pick)
        app.handle_key(key(KeyCode::Down)).unwrap(); // move to "has file"
        app.handle_key(key(KeyCode::Enter)).unwrap(); // choose "has file"
        for c in "Cargo.toml".chars() {
            app.handle_key(key(KeyCode::Char(c))).unwrap();
        }
        app.handle_key(key(KeyCode::Enter)).unwrap(); // commit rule -> autosave
        app.handle_key(key(KeyCode::Enter)).unwrap(); // done -> close Detail, drift recomputes
        assert!(
            t1.elapsed() < std::time::Duration::from_secs(1),
            "editing a rule must not block on a disk walk"
        );
        assert!(
            app.job.is_none() && app.detached.is_empty(),
            "the new atom was already indexed at scan time — editing must not \
             dispatch a background IndexAtoms job"
        );

        // ── Interaction 3: open Apply — rows built entirely from the index ─
        let t2 = std::time::Instant::now();
        app.handle_key(key(KeyCode::Char('w'))).unwrap(); // open Apply
        assert!(
            t2.elapsed() < std::time::Duration::from_secs(1),
            "opening Apply must not block on a disk walk"
        );
        let text = render_profile(&mut app);
        assert!(text.contains("APPLY"), "Apply must render: {text}");
        assert!(
            text.contains("frontend"),
            "the row's matched profile must still be definite: {text}"
        );
        assert!(
            !text.contains("pending index"),
            "a fully indexed row must never render as pending: {text}"
        );
        assert!(
            !text.contains('\u{2026}'),
            "Apply must never show the pending ellipsis for an indexed repo: {text}"
        );
    }
}
