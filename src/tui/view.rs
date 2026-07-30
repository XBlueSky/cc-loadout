use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::Frame;
use time::OffsetDateTime;

use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::Snapshot;

/// An intent a view emits for `App` to execute against the domain layer. Centralizing
/// side effects here keeps views pure (and unit-testable) and error handling in one place.
///
/// Note: `PartialEq`/`Eq` are NOT derived on `Action`. Tests use `matches!` instead.
#[derive(Debug)]
pub enum Action {
    Quit,
    Refresh,
    Switch(String),
    Prime(String),
    RemoveAccount(String),
    RelaunchClaude,
    /// Write the working schedule to disk and update crontab.
    WriteSchedule(std::collections::BTreeMap<String, Vec<String>>),
    /// Run a task now (id), on the job thread.
    RunTask(String),
    /// Remove a task (id), on the job thread.
    RemoveTask(String),
    /// Write profiles.json + global settings + the selected repos' settings.local.json.
    /// Emitted by the Apply sub-view.
    Commit {
        cfg: crate::profile::config::Profiles,
        repos: Vec<std::path::PathBuf>,
    },
    /// Ask Claude to draft the plugin→profile assignment for these scanned inputs.
    DraftWithClaude {
        inv: crate::profile::discover::Inventory,
        scan_roots: Vec<String>,
    },
    /// Walk `roots` for git repos on the job thread (a depth-6 filesystem scan
    /// that must never block the UI); the result is handed back via `accept_scan`.
    /// `working` is carried so the job can also compute the uncovered-repos drift
    /// (post-merge) on the job thread, keeping that walk off the UI thread too.
    Rescan {
        roots: Vec<String>,
        working: crate::profile::config::Profiles,
    },
}

/// One tab. Renders the body area; emits `Action`s rather than touching disk.
pub trait View {
    fn title(&self) -> &str;
    fn on_key(&mut self, key: KeyEvent, ctx: &AppCtx, snap: &Snapshot) -> Option<Action>;
    /// Whether the focused view/mode handles `code` itself, so `App` must NOT
    /// apply its global shortcut for it. Return `true` for every key the view
    /// acts on in its current state (it only MATTERS for the global keys
    /// q/Esc/r/R/Tab/BackTab — non-global keys reach `on_key` regardless).
    /// Default: false (global shortcuts win). Replaces the old
    /// `wants_raw_input` + `wants_key`.
    fn claims_key(&self, _code: KeyCode) -> bool {
        false
    }
    /// Key hints for the footer, as (key, label) pairs. Default: none.
    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
    fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        snap: &Snapshot,
        now_ms: i64,
        now_local: OffsetDateTime,
    );
    /// Receive an AI-generated draft config produced by a background job.
    /// Default: ignore (only the Profile view consumes it).
    fn accept_draft(&mut self, _draft: crate::profile::config::Profiles) {}
    /// Receive a completed background repo scan (from `Action::Rescan`).
    /// Default: ignore (only the Profile view consumes it).
    fn accept_scan(&mut self, _outcome: crate::tui::job::ScanOutcome) {}
    /// Receive a recomputed uncovered-repos set (from a completed `Rescan` job).
    /// Default: ignore (only the Profile view consumes it).
    fn accept_uncovered(&mut self, _uncovered: Vec<String>) {}
    /// Return the working config to persist when it has unsaved edits, marking it
    /// clean. Default: `None` (only the Profile view holds a persistent config).
    /// `App` calls this after every key and job result; `Some(cfg)` triggers an
    /// autosave of profiles.json.
    fn dirty_config(&mut self) -> Option<crate::profile::config::Profiles> {
        None
    }
    /// Return the uncovered-repos set to persist into the scan cache when it has
    /// changed since the last persist, marking it clean. Mirrors `dirty_config`:
    /// `App` calls it after every key and job result so the cache stays the
    /// authoritative uncovered source across sessions, regardless of which code
    /// path recomputed the set. Default: `None` (only the Profile view holds it).
    fn dirty_uncovered(&mut self) -> Option<Vec<String>> {
        None
    }
}
