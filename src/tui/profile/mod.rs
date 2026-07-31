use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::Frame;
use time::OffsetDateTime;

use crate::profile::config::Profiles;
use crate::profile::discover::Inventory;
use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::Snapshot;
use crate::tui::view::{Action, View};

mod apply;
mod assign;
mod board;
mod by_plugin;
pub mod detail;
pub(crate) mod explain;
pub(crate) mod on_demand_help;
pub(crate) mod rules;
#[cfg(test)]
pub(crate) mod test_support;

/// Compact "3d ago" / "2h ago" / "5m ago" / "just now" for a past epoch-seconds
/// value relative to `now_secs`. Shared by the by-plugin scan bar and the
/// Rules tab's index-age tag.
fn fmt_age(then: i64, now_secs: i64) -> String {
    let d = (now_secs - then).max(0);
    if d < 60 {
        "just now".to_string()
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86_400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86_400)
    }
}

/// Which sub-view is active.
enum Sub {
    Board,
    Assign(assign::AssignState),
    Detail(Box<detail::DetailState>),
    Apply(apply::ApplyState),
}

/// Which board view mode is active.
pub enum ViewMode {
    ByPlugin,
    ByProfile,
}

pub struct ProfileView {
    pub(super) inv: Inventory,
    pub(super) working: Profiles,
    pub cursor: usize,
    pub(super) plugin_cursor: usize,
    pub(super) view: ViewMode,
    sub: Sub,
    pub(super) uncovered: Vec<String>,
    pub(super) claude_available: bool,
    pub(super) ai_offer: bool,
    /// Active membership picker (ByPlugin Board only).
    pub(super) pick: Option<by_plugin::MembershipPick>,
    /// Suggested/confirmed scan roots (header summary + `s` scan targets, union).
    /// Seeded by `with_scan_roots`. Empty until set.
    pub(super) scan_roots: Vec<String>,
    /// Active Scan Roots manager (by-plugin Board only).
    pub(super) roots_editor: Option<by_plugin::RootsEdit>,
    /// Whether the static "what is on-demand?" help overlay is open.
    /// ByProfile Board only; opened with `?` on the On-demand row.
    on_demand_help: bool,
    /// Snapshot of `working` as last written to disk; `dirty_config` compares
    /// against it so autosave fires exactly when the config actually changed.
    saved: Profiles,
    /// Snapshot of `uncovered` as last persisted to the scan cache; `dirty_uncovered`
    /// compares against it so a cache write fires exactly when the set changed.
    saved_uncovered: Vec<String>,
    /// Set when the last signal-based recompute (`uncovered_from_signals`) could
    /// not decide every repo — some profile's rule needed an atom the index
    /// never recorded. While true, `self.uncovered` keeps its last value and the
    /// board renders a trailing `…` cue (Task 7) rather than claiming a stale
    /// answer is final.
    pub(super) uncovered_pending: bool,
    /// Epoch seconds of the scan whose repos are loaded (from cache or a live
    /// scan); `None` until anything is scanned. Drives the scan bar's age/stale.
    pub(super) scanned_at: Option<i64>,
    /// Newly-pending atoms drained from an open Detail's `RulesState::wants_index`,
    /// waiting to be dispatched as the next `Action::IndexAtoms` batch. Deduped
    /// on every push (both against what's already queued and within the drained
    /// batch itself) so a delete-then-recommit cycle can never queue the same
    /// atom twice — `RulesState` has no memory of prior pushes, so this is
    /// where duplicates are caught.
    pub(super) index_queue: Vec<String>,
    /// Whether a background `Action::IndexAtoms` job is currently in flight.
    /// While true, a non-empty `index_queue` waits for the NEXT `on_key` call
    /// after `accept_index` clears this flag before dispatching the follow-up
    /// batch (sequential batches, never two jobs racing each other).
    pub(super) indexing: bool,
    /// The atom batch of the currently in-flight `Action::IndexAtoms` job (or
    /// about to be dispatched), for the scan bar / count-line "indexing …" text.
    pub(super) indexing_atoms: Vec<String>,
}

impl ProfileView {
    /// Create a new `ProfileView`.
    ///
    /// - `claude_available`: whether `claude` is on PATH (from `util::claude_on_path()`).
    /// - `offer`: whether this is a first-run setup (i.e. `profiles.json` was absent).
    ///   The consent offer is shown only when BOTH are true.
    pub fn new(inv: Inventory, working: Profiles, claude_available: bool, offer: bool) -> Self {
        let ai_offer = claude_available && offer;
        let saved = working.clone();
        let mut v = ProfileView {
            inv,
            working,
            cursor: 0,
            plugin_cursor: 0,
            view: ViewMode::ByPlugin,
            sub: Sub::Board,
            uncovered: Vec::new(),
            claude_available,
            ai_offer,
            pick: None,
            scan_roots: Vec::new(),
            roots_editor: None,
            on_demand_help: false,
            saved,
            saved_uncovered: Vec::new(),
            uncovered_pending: false,
            scanned_at: None,
            index_queue: Vec::new(),
            indexing: false,
            indexing_atoms: Vec::new(),
        };
        v.recompute_uncovered();
        v.saved_uncovered = v.uncovered.clone();
        v
    }

    fn recompute_uncovered(&mut self) {
        self.uncovered = crate::profile::drift::uncovered_repos(&self.inv, &self.working);
    }

    /// Set the suggested/confirmed scan roots. Used by `App::new`; the header
    /// summarizes them and `s` scans their union.
    pub(crate) fn with_scan_roots(mut self, roots: Vec<String>) -> Self {
        self.scan_roots = roots;
        self
    }

    /// Seed the last scan's timestamp (from the scan cache), so the scan bar can
    /// show its age and staleness immediately on reopen.
    pub(crate) fn with_scanned_at(mut self, at: Option<i64>) -> Self {
        self.scanned_at = at;
        self
    }

    /// Seed repo signals from the scan cache WITHOUT re-walking the filesystem
    /// or recomputing drift. Used by `App::new` so startup stays walk-free:
    /// the expensive per-repo detection already ran at scan time and its result
    /// arrives via `with_uncovered`.
    pub(crate) fn with_scan_repos(
        mut self,
        repos: Vec<crate::profile::discover::RepoSignal>,
    ) -> Self {
        self.inv.repos = repos;
        self
    }

    /// Seed the uncovered-repos drift from the scan cache (the authoritative
    /// value computed at scan time), marking it clean so it is not re-persisted
    /// on the first key. No filesystem I/O — this is the startup fast path that
    /// replaces the synchronous `recompute_uncovered()` walk over every repo.
    pub(crate) fn with_uncovered(mut self, uncovered: Vec<String>) -> Self {
        self.saved_uncovered = uncovered.clone();
        self.uncovered = uncovered;
        self
    }

    /// Mark the seeded `uncovered` set as provisional (a v1 scan cache predates
    /// the atom index, so nothing can be decided from it yet). Task 10 replaces
    /// this seed with an explicit rebuild + banner; until then the board just
    /// renders the pending cue over the empty seeded set.
    pub(crate) fn with_uncovered_pending(mut self, pending: bool) -> Self {
        self.uncovered_pending = pending;
        self
    }

    /// Scan the UNION of `self.scan_roots` for git repos (max depth 6),
    /// repopulating repo signals and suggested profile buckets. New suggested
    /// buckets are merged into `working` (existing profiles/assignments left
    /// untouched). The scanned set is recorded in `working.scan_roots`. No-op
    /// when there are no non-empty roots. Idempotent.
    /// The non-empty scan roots for an explicit scan, or `None` when there is
    /// nothing to walk (so callers can skip emitting a job).
    pub(super) fn nonempty_scan_roots(&self) -> Option<Vec<String>> {
        let roots: Vec<String> = self
            .scan_roots
            .iter()
            .filter(|r| !r.is_empty())
            .cloned()
            .collect();
        (!roots.is_empty()).then_some(roots)
    }

    /// Synchronous scan — walks the roots on the calling thread. Test-only now
    /// (the TUI dispatches `Action::Rescan` so the walk runs on the job thread,
    /// see `apply_scan`), kept as a convenience for exercising `apply_scan`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn scan(&mut self) {
        let Some(roots) = self.nonempty_scan_roots() else {
            return;
        };
        let vocab = crate::profile::signal_detect::vocabulary(&self.working);
        let repos = crate::profile::discover::scan_repo_signals(&roots, 6, &vocab);
        let suggested = crate::profile::discover::suggest_profiles(&repos);
        let uncovered =
            crate::profile::drift::uncovered_post_merge(&self.working, &suggested, &repos);
        self.apply_scan(crate::tui::job::ScanOutcome {
            roots,
            repos,
            suggested,
            uncovered,
            scanned_at: crate::now_epoch(),
        });
    }

    /// Fold a completed scan back into the view: repopulate repo signals and
    /// suggested buckets, merge new suggested profiles into `working` (existing
    /// profiles/assignments untouched), record the scanned roots, and refresh the
    /// uncovered set. Runs on the UI thread but does no filesystem I/O — the walk
    /// already happened on the job thread.
    pub(super) fn apply_scan(&mut self, outcome: crate::tui::job::ScanOutcome) {
        self.inv.suggested_profiles = outcome.suggested;
        self.inv.repos = outcome.repos;
        for sp in &self.inv.suggested_profiles {
            self.working
                .profiles
                .entry(sp.name.clone())
                .or_insert_with(|| {
                    crate::profile::author::profile_from(Vec::new(), &sp.shared_signals)
                });
        }
        self.working.scan_roots = outcome.roots;
        // The uncovered drift was computed on the job thread (post-merge) and
        // handed back in the outcome — assign it directly rather than re-walking
        // every repo on the UI thread. A full rescan is always decisive (it
        // walked every repo live), so it also resolves any pending state left
        // over from a seeded-but-undecided legacy cache.
        self.uncovered = outcome.uncovered;
        self.uncovered_pending = false;
    }

    /// Build the AI-draft action, scanning first when no repos have been
    /// gathered yet so Claude receives real repo context instead of a
    /// universal-only guess. `scan()` is idempotent and a no-op on an empty
    /// scan root, so this is safe whether or not the user already scanned.
    fn draft_action(&mut self) -> Option<Action> {
        // Never scan on the UI thread: the draft job walks the roots in the
        // background (spinner visible) when it has no repo context yet. Prefer
        // the already-scanned roots, else the suggested/confirmed roots.
        let scan_roots = if self.working.scan_roots.is_empty() {
            self.nonempty_scan_roots().unwrap_or_default()
        } else {
            self.working.scan_roots.clone()
        };
        Some(Action::DraftWithClaude {
            inv: self.inv.clone(),
            scan_roots,
        })
    }

    /// Test-visible: assemble the current Drift for assertions.
    #[cfg(test)]
    pub fn drift_for_test(&self, snap: &Snapshot) -> crate::profile::drift::Drift {
        crate::profile::drift::Drift {
            new_unassigned: crate::profile::draft::unassigned_keys(&self.inv, &self.working),
            stale: crate::profile::drift::stale_refs(&self.inv, &self.working),
            uncovered: self.uncovered.clone(),
            global: crate::profile::drift::global_drift(&self.working, &snap.global_enabled),
            scope: snap.scope_drift.clone(),
        }
    }

    /// Test-visible: the navigable row labels in order.
    /// = ["Universal"] ++ working.profiles.keys() (BTreeMap order)
    ///   ++ ["Unassigned", "On-demand"]
    pub fn row_labels(&self) -> Vec<String> {
        let mut labels = vec!["Universal".to_string()];
        labels.extend(self.working.profiles.keys().cloned());
        labels.push("Unassigned".to_string());
        labels.push("On-demand".to_string());
        labels
    }

    /// Test-visible accessor for the in-memory working config.
    #[cfg(test)]
    pub fn working_for_test(&self) -> &Profiles {
        &self.working
    }

    /// Test-visible accessor for the inventory.
    #[cfg(test)]
    pub fn inv_for_test(&self) -> &Inventory {
        &self.inv
    }

    /// Test-visible accessor for the uncovered-repos drift set.
    #[cfg(test)]
    pub fn uncovered_for_test(&self) -> &[String] {
        &self.uncovered
    }

    /// Test-visible accessor for whether the last signal-based recompute was
    /// undecided for some repo (see `uncovered_pending`).
    #[cfg(test)]
    pub fn uncovered_pending_for_test(&self) -> bool {
        self.uncovered_pending
    }

    /// Test-visible accessor for Apply state (returns None if not in Apply).
    #[cfg(test)]
    pub fn apply_state_for_test(&self) -> Option<&apply::ApplyState> {
        match &self.sub {
            Sub::Apply(s) => Some(s),
            _ => None,
        }
    }

    /// Test-visible accessor for the current view mode.
    #[cfg(test)]
    pub fn view_for_test(&self) -> &ViewMode {
        &self.view
    }

    /// Test helper: switch to the ByProfile board (skips the ByPlugin default).
    /// Used by sub-view tests (detail, assign) that need to reach the board
    /// Enter handler before ByPlugin is the active mode.
    #[cfg(test)]
    pub fn switch_to_by_profile_for_test(&mut self) {
        self.view = ViewMode::ByProfile;
    }

    /// Test-visible accessor for the membership picker.
    #[cfg(test)]
    pub fn pick_for_test(&self) -> Option<&by_plugin::MembershipPick> {
        self.pick.as_ref()
    }
}

impl View for ProfileView {
    fn title(&self) -> &str {
        "Profile"
    }

    fn claims_key(&self, code: KeyCode) -> bool {
        // The roots manager is its own overlay: in its text-entry sub-mode it
        // claims everything but Tab/BackTab; in its list sub-mode it claims Esc.
        if let Some(ed) = &self.roots_editor {
            return if ed.input.is_some() {
                !matches!(code, KeyCode::Tab | KeyCode::BackTab)
            } else {
                matches!(code, KeyCode::Esc)
            };
        }
        // The on-demand help overlay (ByProfile Board) claims Esc (close the
        // overlay, not quit) and Tab/BackTab (no tab cycling while open) —
        // the same contract as the explain overlay below.
        if self.on_demand_help {
            return matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab);
        }
        // The explain overlay claims Esc (close overlay, not quit) and Tab/BackTab
        // (no cycling while the overlay is open). This must come BEFORE the
        // builder/picker guard and the rename/naming branch.
        if matches!(&self.sub, Sub::Detail(s) if s.explain.is_some()) {
            return matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab);
        }
        // The Rules-tab add/edit builder owns the WHOLE keyboard while open —
        // including Tab (switch file→word field / no-op in kind-pick) and q/r
        // (literal text or no-ops) and Esc (cancel). Claiming everything here
        // keeps claim-scope == capture-scope so `App` never applies a global
        // shortcut over a key the builder is about to consume. This must come
        // BEFORE the rename/naming branch (which lets Tab escape) and before
        // the `Sub::Detail(_)` global-key arm.
        if matches!(&self.sub, Sub::Detail(s) if s.rules.is_building() || s.rules.is_picking()) {
            return true;
        }
        // Other text-entry sub-modes claim all keys but Tab/BackTab — Tab there
        // genuinely escapes (rename commits-on-Tab semantics, naming pickers).
        let naming = matches!(&self.sub, Sub::Assign(s) if s.naming.is_some())
            || matches!(&self.sub, Sub::Detail(s) if s.renaming.is_some())
            || (matches!(self.view, ViewMode::ByPlugin)
                && self.pick.as_ref().is_some_and(|p| p.naming.is_some()));
        if naming {
            return !matches!(code, KeyCode::Tab | KeyCode::BackTab);
        }
        match &self.sub {
            Sub::Detail(_) => matches!(code, KeyCode::Esc | KeyCode::Tab | KeyCode::Char('r')),
            Sub::Assign(_) => matches!(code, KeyCode::Esc),
            Sub::Apply(_) => matches!(code, KeyCode::Esc),
            Sub::Board => {
                if matches!(self.view, ViewMode::ByPlugin) {
                    if self.pick.is_some() {
                        matches!(code, KeyCode::Esc) // membership picker: Esc cancels
                    } else {
                        matches!(code, KeyCode::Char('r')) // browse: r opens roots manager
                    }
                } else {
                    false // by-profile board claims no global keys
                }
            }
        }
    }

    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.sub {
            Sub::Assign(_) => vec![("↑↓", "choose"), ("⏎", "assign"), ("esc", "back")],
            Sub::Apply(_) => vec![
                ("space", "toggle"),
                ("⏎", "write"),
                ("x", "explain"),
                ("esc", "back"),
            ],
            Sub::Board => match &self.view {
                ViewMode::ByPlugin => {
                    // Scan Roots manager active?
                    if let Some(ed) = &self.roots_editor {
                        if ed.input.is_some() {
                            return vec![("⏎", "confirm"), ("→", "complete"), ("esc", "cancel")];
                        }
                        return vec![
                            ("↑↓", "move"),
                            ("⏎", "edit"),
                            ("a", "add"),
                            ("d", "remove"),
                            ("esc", "done"),
                        ];
                    }
                    // Picker active?
                    if let Some(pick) = &self.pick {
                        if pick.naming.is_some() {
                            return vec![("⏎", "confirm"), ("esc", "cancel")];
                        }
                        return vec![("space", "toggle"), ("⏎", "done"), ("esc", "cancel")];
                    }
                    let mut hints = vec![
                        ("↑↓", "browse"),
                        ("⏎", "assign"),
                        ("v", "by-profile"),
                        ("w", "apply"),
                    ];
                    if self.claude_available {
                        hints.push(("a", "draft"));
                    }
                    hints
                }
                ViewMode::ByProfile => {
                    if self.on_demand_help {
                        return vec![("esc", "close")];
                    }
                    let mut hints = vec![
                        ("↑↓", "select"),
                        ("⏎", "open"),
                        ("s", "scan"),
                        ("w", "apply"),
                    ];
                    if self.claude_available {
                        hints.push(("a", "ai draft"));
                    }
                    if self.row_labels().get(self.cursor).map(String::as_str) == Some("On-demand") {
                        hints.push(("?", "what is on-demand?"));
                    }
                    hints
                }
            },
            Sub::Detail(s) if s.renaming.is_none() => {
                if s.explain.is_some() {
                    vec![("↑↓", "choose repo"), ("esc", "back")]
                } else if s.rules.is_picking() {
                    vec![("↑↓", "choose"), ("⏎", "prefill"), ("esc", "cancel")]
                } else if s.rules.editor.is_some() {
                    if s.rules.building_path_under() {
                        vec![("⏎", "save"), ("→", "complete"), ("esc", "cancel")]
                    } else {
                        vec![("⏎", "save"), ("esc", "cancel"), ("⇥", "field")]
                    }
                } else {
                    match s.focus {
                        detail::DetailFocus::Plugins => vec![
                            ("space", "toggle"),
                            ("⏎", "done"),
                            ("Tab", "plugins/rules"),
                            ("r", "rename"),
                            ("del", "delete"),
                        ],
                        detail::DetailFocus::Rules => vec![
                            ("a", "add"),
                            ("e", "edit"),
                            ("d", "del"),
                            ("f", "from repo"),
                            ("?", "explain"),
                            ("⏎", "done"),
                            ("Tab", "plugins/rules"),
                        ],
                    }
                }
            }
            Sub::Detail(_) => vec![("⏎", "confirm"), ("esc", "cancel")],
        }
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &AppCtx, _snap: &Snapshot) -> Option<Action> {
        // For Assign and Detail we need to detect "done" and then recompute
        // uncovered after the match arm releases the borrow on self.sub.
        let mut recompute = false;
        let mut detail_done = false;
        let mut action_out: Option<Action> = None;
        let mut close_roots: Option<Vec<String>> = None;

        match &mut self.sub {
            Sub::Board => {
                if let Some(ed) = self.roots_editor.as_mut() {
                    if ed.input.is_some() {
                        match key.code {
                            KeyCode::Enter => {
                                let val = ed.input.as_ref().unwrap().value().trim().to_string();
                                if !val.is_empty() {
                                    match ed.edit_idx {
                                        Some(i) => {
                                            if !ed
                                                .roots
                                                .iter()
                                                .enumerate()
                                                .any(|(j, r)| j != i && r == &val)
                                            {
                                                ed.roots[i] = val;
                                            }
                                        }
                                        None => {
                                            if !ed.roots.contains(&val) {
                                                ed.roots.push(val);
                                            }
                                        }
                                    }
                                }
                                ed.input = None;
                                ed.edit_idx = None;
                                ed.suggestion = None;
                            }
                            KeyCode::Esc => {
                                ed.input = None;
                                ed.edit_idx = None;
                                ed.suggestion = None;
                            }
                            KeyCode::Right
                                if ed.input.as_ref().unwrap().at_end()
                                    && ed.suggestion.is_some() =>
                            {
                                let suff = ed.suggestion.take().unwrap();
                                let val = ed.input.as_ref().unwrap().value() + &suff;
                                ed.suggestion = by_plugin::dir_suggestion(&val);
                                ed.input = Some(crate::tui::textinput::TextInput::new(&val));
                            }
                            _ => {
                                ed.input.as_mut().unwrap().handle_key(key);
                                let val = ed.input.as_ref().unwrap().value();
                                ed.suggestion = by_plugin::dir_suggestion(&val);
                            }
                        }
                    } else {
                        let n = ed.roots.len() + 1; // + the "add" row
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') => {
                                ed.cursor = (ed.cursor + n - 1) % n;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                ed.cursor = (ed.cursor + 1) % n;
                            }
                            KeyCode::Char('a') => {
                                let seed = format!("{}/", ctx.home.display());
                                ed.suggestion = by_plugin::dir_suggestion(&seed);
                                ed.input = Some(crate::tui::textinput::TextInput::new(&seed));
                                ed.edit_idx = None;
                            }
                            KeyCode::Char('d') | KeyCode::Delete if ed.cursor < ed.roots.len() => {
                                ed.roots.remove(ed.cursor);
                                ed.cursor = ed.cursor.min(ed.roots.len());
                            }
                            KeyCode::Enter => {
                                if ed.cursor < ed.roots.len() {
                                    let seed = ed.roots[ed.cursor].clone();
                                    ed.suggestion = by_plugin::dir_suggestion(&seed);
                                    ed.input = Some(crate::tui::textinput::TextInput::new(&seed));
                                    ed.edit_idx = Some(ed.cursor);
                                } else {
                                    let seed = format!("{}/", ctx.home.display());
                                    ed.suggestion = by_plugin::dir_suggestion(&seed);
                                    ed.input = Some(crate::tui::textinput::TextInput::new(&seed));
                                    ed.edit_idx = None;
                                }
                            }
                            KeyCode::Esc => {
                                close_roots = Some(std::mem::take(&mut ed.roots));
                            }
                            _ => {}
                        }
                    }
                } else {
                    // On-demand help overlay open (ByProfile Board): Esc
                    // closes; every other key is swallowed so Board keys
                    // (ai-offer y/n, a/s/v/w/…) cannot fire underneath the
                    // overlay.
                    if self.on_demand_help {
                        if key.code == KeyCode::Esc {
                            self.on_demand_help = false;
                        }
                        return None;
                    }

                    // While the AI consent offer is showing, y/n are high-priority.
                    if self.ai_offer {
                        match key.code {
                            KeyCode::Char('y') => {
                                self.ai_offer = false;
                                return self.draft_action();
                            }
                            KeyCode::Char('n') => {
                                self.ai_offer = false;
                                return None;
                            }
                            _ => {}
                        }
                    }

                    // 'a' key — trigger AI draft any time on Board when claude is available.
                    if self.claude_available && key.code == KeyCode::Char('a') {
                        self.ai_offer = false;
                        return self.draft_action();
                    }

                    // 's' — explicit, skippable repo scan. Fires in either view mode
                    // while no membership picker is open (the picker has its own keys).
                    // The depth-6 filesystem walk runs on the job thread (Action::
                    // Rescan) so the event loop keeps animating a spinner; a no-op
                    // when there are no roots to walk.
                    if key.code == KeyCode::Char('s') && self.pick.is_none() {
                        let working = self.working.clone();
                        return self.nonempty_scan_roots().map(|roots| Action::Rescan {
                            roots,
                            working: working.clone(),
                        });
                    }

                    match &self.view {
                        ViewMode::ByPlugin => {
                            // ── Picker active ─────────────────────────────────────
                            if let Some(pick) = self.pick.as_mut() {
                                if pick.naming.is_some() {
                                    // Naming sub-mode: feed key to TextInput.
                                    match key.code {
                                        KeyCode::Enter => {
                                            let raw = pick.naming.as_ref().unwrap().value();
                                            let name = raw.trim().to_string();
                                            if !name.is_empty()
                                                && !self.working.profiles.contains_key(&name)
                                            {
                                                // Insert the empty profile.
                                                self.working
                                                    .profiles
                                                    .entry(name.clone())
                                                    .or_default();
                                                // Insert before the "+ New profile…" sentinel.
                                                let sentinel_pos = pick
                                                    .targets
                                                    .iter()
                                                    .position(|t| t == by_plugin::NEW_PROFILE)
                                                    .unwrap_or(pick.targets.len());
                                                pick.targets.insert(sentinel_pos, name);
                                                pick.checked.insert(sentinel_pos, true);
                                                // Move cursor to the newly created profile.
                                                pick.cursor = sentinel_pos;
                                            }
                                            pick.naming = None;
                                        }
                                        KeyCode::Esc => {
                                            pick.naming = None;
                                        }
                                        _ => {
                                            pick.naming.as_mut().unwrap().handle_key(key);
                                        }
                                    }
                                } else {
                                    // Normal picker mode.
                                    let n = pick.targets.len();
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            pick.cursor = (pick.cursor + n - 1) % n;
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            pick.cursor = (pick.cursor + 1) % n;
                                        }
                                        KeyCode::Char(' ') => {
                                            let cur = pick.cursor;
                                            let target = pick.targets[cur].clone();
                                            if target == by_plugin::NEW_PROFILE {
                                                // Open naming.
                                                pick.naming =
                                                    Some(crate::tui::textinput::TextInput::new(""));
                                            } else if target == "Universal" {
                                                // Toggle Universal ON → clear all profile checks.
                                                let was_checked = pick.checked[cur];
                                                pick.checked[cur] = !was_checked;
                                                if pick.checked[cur] {
                                                    // Clear all non-Universal checks.
                                                    for (i, t) in pick.targets.iter().enumerate() {
                                                        if i != cur && t != by_plugin::NEW_PROFILE {
                                                            pick.checked[i] = false;
                                                        }
                                                    }
                                                }
                                            } else {
                                                // A profile target: toggle it; if turning ON, clear Universal.
                                                let was_checked = pick.checked[cur];
                                                pick.checked[cur] = !was_checked;
                                                if pick.checked[cur] {
                                                    // Clear Universal (index 0).
                                                    pick.checked[0] = false;
                                                }
                                            }
                                        }
                                        KeyCode::Enter => {
                                            let cur = pick.cursor;
                                            let target = pick.targets[cur].clone();
                                            if target == by_plugin::NEW_PROFILE {
                                                // ⏎ on sentinel → open naming (same as space).
                                                pick.naming =
                                                    Some(crate::tui::textinput::TextInput::new(""));
                                            } else {
                                                // Commit: rewrite membership from checked state.
                                                let plugin_key = pick.key.clone();
                                                let targets = pick.targets.clone();
                                                let checked = pick.checked.clone();

                                                // Remove key from universal and all profiles.
                                                self.working.universal.retain(|k| k != &plugin_key);
                                                for p in self.working.profiles.values_mut() {
                                                    p.plugins.retain(|k| k != &plugin_key);
                                                }
                                                // Re-add per checked targets.
                                                for (i, t) in targets.iter().enumerate() {
                                                    if !checked[i] || t == by_plugin::NEW_PROFILE {
                                                        continue;
                                                    }
                                                    if t == "Universal" {
                                                        if !self
                                                            .working
                                                            .universal
                                                            .contains(&plugin_key)
                                                        {
                                                            self.working
                                                                .universal
                                                                .push(plugin_key.clone());
                                                        }
                                                    } else if let Some(p) =
                                                        self.working.profiles.get_mut(t)
                                                    {
                                                        if !p.plugins.contains(&plugin_key) {
                                                            p.plugins.push(plugin_key.clone());
                                                        }
                                                    }
                                                }
                                                // Keep on_demand and universal/profiles
                                                // disjoint: if this commit assigned the plugin
                                                // to any managed target, it is no longer an
                                                // ad-hoc borrow — drop it from on_demand. If
                                                // nothing was checked, leave on_demand alone
                                                // (no silent eviction).
                                                let assigned =
                                                    targets.iter().zip(&checked).any(|(t, c)| {
                                                        *c && t != by_plugin::NEW_PROFILE
                                                    });
                                                if assigned {
                                                    self.working
                                                        .on_demand
                                                        .retain(|k| k != &plugin_key);
                                                }
                                                self.pick = None;
                                                // No uncovered recompute: membership only changes
                                                // which plugins a profile carries, never its detect
                                                // rules — so repo coverage is unaffected.
                                            }
                                        }
                                        KeyCode::Esc => {
                                            self.pick = None;
                                        }
                                        _ => {}
                                    }
                                }
                                // Picker consumed the key — skip browse handling below.
                                return action_out;
                            }

                            // ── No picker: normal ByPlugin browse ─────────────────
                            let n = self.inv.plugins.len();
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                                    self.plugin_cursor = (self.plugin_cursor + n - 1) % n;
                                }
                                KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                                    self.plugin_cursor = (self.plugin_cursor + 1) % n;
                                }
                                KeyCode::Char('v') => {
                                    self.view = ViewMode::ByProfile;
                                }
                                KeyCode::Char('r') => {
                                    self.roots_editor =
                                        Some(by_plugin::RootsEdit::open(&self.scan_roots));
                                }
                                KeyCode::Enter if n > 0 => {
                                    let key_str = self.inv.plugins[self.plugin_cursor].key.clone();
                                    self.pick = Some(by_plugin::MembershipPick::open(
                                        &self.working,
                                        &key_str,
                                    ));
                                }
                                KeyCode::Char('w') => {
                                    let state =
                                        apply::ApplyState::open(&self.inv.repos, &self.working);
                                    self.sub = Sub::Apply(state);
                                }
                                _ => {}
                            }
                        }
                        ViewMode::ByProfile => {
                            if key.code == KeyCode::Char('v') {
                                self.view = ViewMode::ByPlugin;
                            } else {
                                let n = self.row_labels().len();
                                match key.code {
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        self.cursor = (self.cursor + n - 1) % n;
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        self.cursor = (self.cursor + 1) % n;
                                    }
                                    KeyCode::Enter => {
                                        let labels = self.row_labels();
                                        let unassigned_idx = labels
                                            .iter()
                                            .position(|l| l == "Unassigned")
                                            .unwrap_or(usize::MAX);
                                        if self.cursor == unassigned_idx {
                                            // Open Assign sub-view for unassigned plugins.
                                            let queue = crate::profile::draft::unassigned_keys(
                                                &self.inv,
                                                &self.working,
                                            );
                                            if !queue.is_empty() {
                                                self.sub =
                                                    Sub::Assign(assign::AssignState::new(queue));
                                            }
                                        } else if self.cursor > 0 && self.cursor < unassigned_idx {
                                            // A named profile row (not Universal at index 0).
                                            let name = labels[self.cursor].clone();
                                            if self.working.profiles.contains_key(&name) {
                                                self.sub = Sub::Detail(Box::new(
                                                    detail::DetailState::open(
                                                        &name,
                                                        &self.inv,
                                                        &self.working,
                                                    ),
                                                ));
                                            }
                                        }
                                    }
                                    KeyCode::Char('w') => {
                                        // Open the Apply sub-view.
                                        let state =
                                            apply::ApplyState::open(&self.inv.repos, &self.working);
                                        self.sub = Sub::Apply(state);
                                    }
                                    KeyCode::Char('?') => {
                                        // Only the On-demand row (always last
                                        // in row_labels()) opens the help.
                                        let labels = self.row_labels();
                                        if labels.get(self.cursor).map(String::as_str)
                                            == Some("On-demand")
                                        {
                                            self.on_demand_help = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                } // end else (manager not open)
            }
            Sub::Assign(state) => {
                let done = state.handle_key(key, &mut self.working);
                if done {
                    recompute = true;
                }
            }
            Sub::Detail(state) => {
                let done = state.handle_key(key, &self.inv, &mut self.working);
                if done {
                    detail_done = true;
                    // Only editing detect rules (or deleting the profile) changes
                    // which repos are uncovered; editing plugins or renaming does
                    // not, so skip the expensive all-repos recompute in that case.
                    recompute = state.detection_changed();
                }
            }
            Sub::Apply(state) => {
                let outcome = state.handle_key(key, &self.working);
                match outcome {
                    apply::ApplyOutcome::None => {}
                    apply::ApplyOutcome::Back => {
                        self.sub = Sub::Board;
                    }
                    apply::ApplyOutcome::Commit(action) => {
                        self.sub = Sub::Board;
                        action_out = Some(action);
                    }
                }
            }
        }

        if let Some(roots) = close_roots {
            // Commit the manager's list (dedup, order-preserving) to the working set.
            let mut seen = std::collections::BTreeSet::new();
            let deduped: Vec<String> = roots
                .into_iter()
                .filter(|r| !r.is_empty() && seen.insert(r.clone()))
                .collect();
            self.scan_roots = deduped.clone();
            self.working.scan_roots = deduped;
            self.roots_editor = None;
            // No uncovered recompute: editing scan roots changes neither the
            // already-scanned repo set nor any detect rule, so coverage is
            // unchanged until the next explicit `s` rescan.
        }

        // Return to the Board after a Detail edit, whether or not detection
        // changed (the recompute below is what's conditional, not the nav).
        if detail_done {
            self.sub = Sub::Board;
        }
        // When detection changed, the uncovered set is recomputed in place from
        // the indexed signal (signal_detect over self.inv.repos) — zero
        // filesystem I/O, so this runs synchronously, no job/spinner needed.
        if recompute {
            self.sub = Sub::Board;
            let (unc, pending) =
                crate::profile::drift::uncovered_from_signals(&self.inv.repos, &self.working);
            if !pending {
                self.uncovered = unc;
            }
            self.uncovered_pending = pending;
        }

        // Task 8: drain any newly-pending atoms an open Detail's Rules tab just
        // queued (a builder commit or `f`-derive can introduce one whether or
        // not `detection_changed()` fired this key) into the index queue, then
        // dispatch a background `Action::IndexAtoms` batch once idle. Dedupe
        // both the incoming batch and against what's already queued — `RulesState`
        // has no memory of prior pushes, so a delete-then-recommit cycle would
        // otherwise queue the same atom twice.
        if let Sub::Detail(state) = &mut self.sub {
            if !state.rules.wants_index.is_empty() {
                let mut seen: std::collections::BTreeSet<String> =
                    self.index_queue.iter().cloned().collect();
                for atom in std::mem::take(&mut state.rules.wants_index) {
                    if seen.insert(atom.clone()) {
                        self.index_queue.push(atom);
                    }
                }
            }
        }
        // A batch already queued while a job was in flight waits here — the
        // next `on_key` call after `accept_index` clears `indexing` dispatches
        // it, so batches are always sequential, never racing each other.
        // `action_out.is_none()` lets an Apply commit (the only other action
        // this function can emit) win if both happen to land on the same key.
        if action_out.is_none() && !self.index_queue.is_empty() && !self.indexing {
            let atoms = std::mem::take(&mut self.index_queue);
            let repos: Vec<String> = self.inv.repos.iter().map(|r| r.path.clone()).collect();
            self.indexing = true;
            self.indexing_atoms = atoms.clone();
            if let Sub::Detail(state) = &mut self.sub {
                state.rules.indexing = true;
            }
            action_out = Some(Action::IndexAtoms { atoms, repos });
        }

        action_out
    }

    fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        snap: &Snapshot,
        now_ms: i64,
        _now_local: OffsetDateTime,
    ) {
        match &self.sub {
            Sub::Board => match &self.view {
                ViewMode::ByPlugin => {
                    if let Some(ed) = &self.roots_editor {
                        by_plugin::render_roots_manager(ed, f, area);
                    } else if let Some(pick) = &self.pick {
                        by_plugin::render_picker(pick, f, area);
                    } else {
                        by_plugin::render(self, f, area, now_ms);
                    }
                }
                ViewMode::ByProfile => {
                    if self.on_demand_help {
                        on_demand_help::render(f, area);
                    } else {
                        board::render(self, snap, f, area)
                    }
                }
            },
            Sub::Assign(state) => assign::render(state, &self.working, f, area),
            Sub::Detail(state) => {
                detail::render(state, &self.inv, f, area, now_ms, self.scanned_at)
            }
            Sub::Apply(state) => apply::render(state, &self.working, f, area),
        }
    }

    fn accept_draft(&mut self, draft: crate::profile::config::Profiles) {
        self.working = draft;
        self.ai_offer = false;
        // The draft replaces every profile's detect rules, so coverage changes —
        // recompute it in place from the indexed signal (zero filesystem I/O).
        let (unc, pending) =
            crate::profile::drift::uncovered_from_signals(&self.inv.repos, &self.working);
        if !pending {
            self.uncovered = unc;
        }
        self.uncovered_pending = pending;
    }

    fn accept_scan(&mut self, outcome: crate::tui::job::ScanOutcome) {
        self.scanned_at = Some(outcome.scanned_at);
        self.apply_scan(outcome);
    }

    fn accept_uncovered(&mut self, uncovered: Vec<String>) {
        self.uncovered = uncovered;
    }

    fn accept_index(&mut self, o: crate::tui::job::IndexOutcome) {
        for repo in &mut self.inv.repos {
            if let Some(hits) = o.hits.get(&repo.path) {
                for (atom, hit) in hits {
                    repo.rule_hits.insert(atom.clone(), *hit);
                }
            }
        }
        // An atom re-queued while THIS batch was still in flight (the
        // delete-then-recommit case) is now redundant — this delivery already
        // answered it. Drop it so the next `on_key` doesn't dispatch a
        // pointless follow-up for an atom that's already indexed.
        self.index_queue.retain(|a| !o.atoms.contains(a));
        self.indexing = false;
        self.indexing_atoms.clear();
        if let Sub::Detail(state) = &mut self.sub {
            state.rules.indexing = false;
            // Refresh the cached match/near-miss preview + pending-atom set
            // against the now-merged index, so the count line reflects the
            // just-answered atom immediately rather than on the next edit.
            state.rules.recompute(&self.inv);
        }
        // Detection didn't change (no rule was added/removed), but the index
        // backing it did — recompute uncovered the same zero-I/O way as any
        // other signal update.
        let (unc, pending) =
            crate::profile::drift::uncovered_from_signals(&self.inv.repos, &self.working);
        if !pending {
            self.uncovered = unc;
        }
        self.uncovered_pending = pending;
    }

    fn dirty_config(&mut self) -> Option<Profiles> {
        if self.working != self.saved {
            self.saved = self.working.clone();
            Some(self.working.clone())
        } else {
            None
        }
    }

    fn dirty_uncovered(&mut self) -> Option<Vec<String>> {
        if self.uncovered != self.saved_uncovered {
            self.saved_uncovered = self.uncovered.clone();
            Some(self.uncovered.clone())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};
    use crate::tui::profile::test_support;
    use ratatui::crossterm::event::KeyEvent;

    // ── Task 4: AI consent offer + 'a' key + accept_draft ────────────────────

    fn inv_one_plugin() -> Inventory {
        Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![],
                shared_signals: SharedSignals::default(),
            }],
        }
    }

    #[test]
    fn ai_offer_y_emits_draft_action_and_clears_offer() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        let mut v = ProfileView::new(inv, working, true, true);
        assert!(v.ai_offer, "ai_offer must be true on first-run with claude");
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('y')), &c, &s);
        assert!(
            matches!(action, Some(Action::DraftWithClaude { .. })),
            "y must emit DraftWithClaude"
        );
        assert!(!v.ai_offer, "ai_offer must be cleared after y");
    }

    #[test]
    fn ai_offer_n_clears_offer_without_action() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        let mut v = ProfileView::new(inv, working, true, true);
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('n')), &c, &s);
        assert!(action.is_none(), "n must not emit an action");
        assert!(!v.ai_offer, "ai_offer must be cleared after n");
    }

    #[test]
    fn a_key_emits_draft_when_claude_available() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        // offer=false so the offer isn't showing, but claude_available=true
        let mut v = ProfileView::new(inv, working, true, false);
        assert!(!v.ai_offer, "ai_offer must be false when offer=false");
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s);
        assert!(
            matches!(action, Some(Action::DraftWithClaude { .. })),
            "a must emit DraftWithClaude when claude is available"
        );
    }

    #[test]
    fn a_key_does_not_scan_on_the_ui_thread_before_drafting() {
        // The pre-draft repo scan must move to the job thread (the draft job
        // scans in the background with the spinner visible). Pressing 'a' with a
        // real git repo under the scan root must emit DraftWithClaude carrying
        // that root, but must NOT walk the filesystem synchronously.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), true, false)
            .with_scan_roots(vec![dir.path().display().to_string()]);
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s);
        match action {
            Some(Action::DraftWithClaude { scan_roots, .. }) => assert_eq!(
                scan_roots,
                vec![dir.path().display().to_string()],
                "draft must carry the roots for the background scan"
            ),
            other => panic!("'a' must emit DraftWithClaude, got {other:?}"),
        }
        assert!(
            v.inv_for_test().repos.is_empty(),
            "'a' must not scan the filesystem on the UI thread"
        );
    }

    #[test]
    fn accept_draft_replaces_working() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        let mut v = ProfileView::new(inv.clone(), working, false, false);
        let replacement = crate::profile::config::Profiles {
            universal: vec!["unique@marker".to_string()],
            ..Default::default()
        };
        v.accept_draft(replacement);
        assert_eq!(
            v.working_for_test().universal,
            vec!["unique@marker".to_string()],
            "accept_draft must replace working with the draft"
        );
        assert!(!v.ai_offer, "accept_draft must clear ai_offer");
    }

    #[test]
    fn accept_draft_recomputes_uncovered_from_the_index_not_the_disk() {
        // accept_draft changes every detect rule, so coverage must be
        // recomputed — but there is no filesystem walk to wait on: the repo's
        // directory below does not exist on disk, only its indexed signal says
        // Cargo.toml is present. A stale disk-walking recompute would find
        // nothing and leave the repo uncovered; the signal-based recompute
        // must pick it up synchronously, no job required.
        use crate::profile::discover::RepoSignal;
        let repo = RepoSignal {
            path: "/does/not/exist/a".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: [("file:Cargo.toml".to_string(), true)]
                .into_iter()
                .collect(),
            override_names: None,
        };
        let inv = Inventory {
            plugins: vec![],
            repos: vec![repo],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false);
        assert_eq!(
            v.uncovered_for_test(),
            &["/does/not/exist/a".to_string()],
            "no profiles yet: the repo is uncovered"
        );

        let draft: Profiles = serde_json::from_str(
            r#"{"universal": [], "profiles": {
                "rust": {"plugins": [], "detect": {"marker_files": ["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        v.accept_draft(draft);
        assert!(
            v.uncovered_for_test().is_empty(),
            "the indexed marker_file hit must cover the repo synchronously"
        );
        assert!(
            !v.uncovered_pending_for_test(),
            "a fully indexed repo must not be pending"
        );
    }

    #[test]
    fn dirty_config_reports_change_once_then_clean() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        let mut v = ProfileView::new(inv, working, false, false);
        assert!(v.dirty_config().is_none(), "clean right after construction");
        v.working.universal.push("newplug@m".into()); // simulate an edit
        assert!(v.dirty_config().is_some(), "reports the change once");
        assert!(v.dirty_config().is_none(), "clean again after reporting");
    }

    #[test]
    fn scan_bar_shows_relative_age_and_stale_warning() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![crate::profile::discover::RepoSignal {
                path: "/workspace/a".into(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested_profiles: vec![],
        };
        let now_secs = 2_000_000_000i64;
        let now_ms = now_secs * 1000;
        let now_local = time::OffsetDateTime::from_unix_timestamp(now_secs).unwrap();
        let snap = test_support::snap();

        let render = |scanned_at: i64| -> String {
            let v = ProfileView::new(inv.clone(), Profiles::default(), false, false)
                .with_scan_roots(vec!["/workspace".into()])
                .with_scanned_at(Some(scanned_at));
            let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
            t.draw(|f| v.render(f, f.area(), &snap, now_ms, now_local))
                .unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        let fresh = render(now_secs - 3600); // 1h ago
        assert!(fresh.contains("1 repos"), "shows repo count: {fresh}");
        assert!(
            fresh.contains("scanned") && fresh.contains("ago"),
            "shows relative age: {fresh}"
        );
        assert!(
            !fresh.contains("may be stale"),
            "fresh scan is not stale: {fresh}"
        );

        let stale = render(now_secs - (8 * 24 * 60 * 60)); // 8 days ago
        assert!(
            stale.contains("may be stale"),
            "old scan warns stale: {stale}"
        );
    }

    #[test]
    fn no_offer_when_claude_unavailable() {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        // claude_available=false even though offer=true: offer must be suppressed
        let mut v = ProfileView::new(inv, working, false, true);
        assert!(
            !v.ai_offer,
            "ai_offer must be false when claude is unavailable"
        );
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s);
        assert!(
            action.is_none(),
            "a must do nothing when claude is unavailable"
        );
    }

    // ── Pre-existing tests (updated call sites to 4-arg new) ─────────────────

    #[test]
    fn drift_surfaces_new_unassigned_stale_global_and_scope() {
        // installed: serena (universal), eslint (unassigned). config references gone@x (stale).
        let inv = Inventory {
            plugins: vec![
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "eslint@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let working: crate::profile::config::Profiles = serde_json::from_str(
            r#"{"universal":["serena@x"],"profiles":{"rust":{"plugins":["gone@x"],"detect":{}}}}"#,
        )
        .unwrap();
        let v = ProfileView::new(inv, working, false, false);
        // global has the profile plugin "gone@x" enabled (drift) — provide via a snapshot.
        let mut s = test_support::snap();
        s.global_enabled = vec!["gone@x".to_string()];
        // scope drift is computed off-band from registry.rs; a snapshot just carries
        // the result through, so a fixed key is enough to prove the wiring.
        s.scope_drift = vec!["cc-loadout@cc-loadout".to_string()];
        let d = v.drift_for_test(&s);
        assert_eq!(d.new_unassigned, vec!["eslint@x".to_string()]);
        assert_eq!(d.stale, vec!["gone@x".to_string()]);
        assert_eq!(d.global, vec!["gone@x".to_string()]);
        assert_eq!(d.scope, vec!["cc-loadout@cc-loadout".to_string()]);
        assert_eq!(d.review_count(), 4);
    }

    fn view() -> ProfileView {
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![],
                shared_signals: SharedSignals::default(),
            }],
        };
        let working = crate::profile::draft::scan_draft(&inv, vec!["/r".into()]);
        ProfileView::new(inv, working, false, false)
    }

    #[test]
    fn board_rows_are_universal_profiles_unassigned_on_demand() {
        let v = view();
        assert_eq!(
            v.row_labels(),
            vec![
                "Universal".to_string(),
                "rust".to_string(),
                "Unassigned".to_string(),
                "On-demand".to_string()
            ]
        );
    }

    #[test]
    fn down_arrow_moves_cursor_with_wrap() {
        let mut v = view();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        // Default is ByPlugin; switch to ByProfile to test board cursor.
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        assert_eq!(v.cursor, 0);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        assert_eq!(v.cursor, 1);
        for _ in 0..3 {
            v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        }
        assert_eq!(v.cursor, 0); // wrapped (4 rows: Universal/rust/Unassigned/On-demand)
    }

    #[test]
    fn up_arrow_wraps_from_zero() {
        let mut v = view();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        // Default is ByPlugin; switch to ByProfile to test board cursor.
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        assert_eq!(v.cursor, 0);
        v.on_key(KeyEvent::from(KeyCode::Up), &c, &s);
        assert_eq!(v.cursor, 3); // wrapped to last row (4 rows)
    }

    #[test]
    fn j_k_navigate_same_as_arrow_keys() {
        let mut v = view();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        // Default is ByPlugin; switch to ByProfile to test board cursor.
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Char('j')), &c, &s);
        assert_eq!(v.cursor, 1);
        v.on_key(KeyEvent::from(KeyCode::Char('k')), &c, &s);
        assert_eq!(v.cursor, 0);
    }

    // ── claims_key tests ────────────────────────────────────────────────────

    #[test]
    fn claims_key_true_while_building_a_rule() {
        // Open Detail, focus Rules, open builder. While a builder is open the
        // builder owns the ENTIRE keyboard (claim-scope == capture-scope), so
        // claims_key must return true for every key — including the global
        // shortcuts q / Tab / Esc — at BOTH the kind-pick and value-entry steps.
        let mut v = view(); // existing helper with a "rust" profile
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s); // -> ByProfile
        v.cursor = 1; // rust row
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s); // open Detail
        v.on_key(KeyEvent::from(KeyCode::Tab), &c, &s); // -> Rules
        v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s); // open builder (kind-pick)

        // Kind-pick step: 'q' must be claimed (bug #2: 'q' used to quit here).
        assert!(
            v.claims_key(KeyCode::Char('q')),
            "kind-pick step claims 'q' (must not quit the app)"
        );
        assert!(
            v.claims_key(KeyCode::Tab),
            "kind-pick step claims Tab (no-op, must not cycle tabs)"
        );

        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s); // into value-entry

        // Value-entry step: every key is claimed, INCLUDING Tab — the builder
        // uses Tab to switch the file→word field (bug #1: Tab used to cycle the
        // top-level tab before the builder could see it).
        assert!(
            v.claims_key(KeyCode::Char('q')),
            "typing a rule value claims 'q'"
        );
        assert!(
            v.claims_key(KeyCode::Tab),
            "the builder claims Tab so it can switch the file→word field"
        );
    }

    #[test]
    fn claims_key_true_only_during_text_entry() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Board — no text entry active.
        assert!(
            !v.claims_key(KeyCode::Char('x')),
            "Board: does not claim 'x'"
        );

        // Switch to ByProfile (default is now ByPlugin).
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);

        // Open Assign on the Unassigned row.
        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // Assign open but naming not yet active.
        assert!(
            !v.claims_key(KeyCode::Char('x')),
            "Assign (no naming): does not claim 'x'"
        );

        // Move to "+ New profile…" (index 3: Universal=0, rust=1, On-demand=2, New=3).
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        // Enter → naming mode active.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        assert!(
            v.claims_key(KeyCode::Char('x')),
            "Assign with naming active: claims 'x'"
        );
    }

    #[test]
    fn assign_esc_returns_to_board() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Switch to ByProfile (default is now ByPlugin).
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);

        // Open Assign.
        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // Verify we're in Assign.
        assert!(
            matches!(&v.sub, Sub::Assign(_)),
            "should be in Assign after Enter on Unassigned"
        );

        // Press Esc → return to Board.
        v.on_key(KeyEvent::from(KeyCode::Esc), &c, &s);

        assert!(
            matches!(&v.sub, Sub::Board),
            "should be back on Board after Esc in Assign"
        );
    }

    // ── Assign sub-view tests ────────────────────────────────────────────────

    /// Build a ProfileView where `working` has profile "rust" (empty plugins)
    /// and serena@x + eslint@x are both unassigned.
    fn view_with_two_unassigned() -> ProfileView {
        let inv = Inventory {
            plugins: vec![
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "eslint@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![],
                shared_signals: SharedSignals::default(),
            }],
        };
        // Neither "serena" nor "eslint" matches "rust", so both remain unassigned.
        let working = crate::profile::draft::scan_draft(&inv, vec![]);
        ProfileView::new(inv, working, false, false)
    }

    #[test]
    fn row_labels_include_on_demand_after_unassigned() {
        let mut v = view_with_two_unassigned(); // existing helper
        v.working.on_demand.push("pixijs@x".to_string());
        let labels = v.row_labels();
        assert_eq!(labels.last(), Some(&"On-demand".to_string()));
        assert_eq!(
            labels[labels.len() - 2],
            "Unassigned".to_string(),
            "On-demand must come right after Unassigned"
        );
    }

    #[test]
    fn assign_puts_plugin_into_chosen_profile_and_advances() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Switch to ByProfile (default is now ByPlugin).
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);

        // Open Assign on the Unassigned row.
        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // targets: ["Universal", "rust", "+ New profile…", "Leave unassigned"]
        // cursor=0 (Universal). Down -> cursor=1 (rust).
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);

        // Enter -> assign first queued plugin (serena@x, alphabetically first) to "rust".
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // unassigned_keys returns sorted: ["eslint@x", "serena@x"].
        // The first one placed should be "eslint@x".
        assert!(
            v.working_for_test().profiles["rust"]
                .plugins
                .contains(&"eslint@x".to_string()),
            "eslint@x (first in sorted queue) should be in rust profile after assignment"
        );
    }

    #[test]
    fn new_profile_target_creates_profile_with_the_plugin() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Switch to ByProfile (default is now ByPlugin).
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);

        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // targets: ["Universal", "rust", "On-demand", "+ New profile…", "Leave unassigned"]
        // Down three times -> cursor=3 ("+ New profile…").
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);

        // Enter -> naming mode.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // Type "embedded".
        for ch in "embedded".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(ch)), &c, &s);
        }

        // Enter -> commit new profile.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.profiles.contains_key("embedded"),
            "embedded profile should have been created"
        );
        // eslint@x is first in the sorted queue.
        assert!(
            w.profiles["embedded"]
                .plugins
                .contains(&"eslint@x".to_string()),
            "eslint@x (first in sorted queue) should be in embedded profile"
        );
    }

    #[test]
    fn leave_unassigned_advances_without_placing() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Switch to ByProfile (default is now ByPlugin).
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);

        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // Move to "Leave unassigned" (index 4 in 5-item list).
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);

        // Enter -> leave unassigned, advance.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // eslint@x is first in sorted queue; it was left unassigned.
        let managed = crate::profile::plugins::managed_keys(v.working_for_test());
        assert!(
            !managed.contains(&"eslint@x".to_string()),
            "eslint@x should remain unmanaged after Leave unassigned"
        );
    }

    #[test]
    fn on_demand_target_adds_plugin_to_on_demand_list() {
        let mut v = view_with_two_unassigned();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        v.cursor = v
            .row_labels()
            .iter()
            .position(|r| r == "Unassigned")
            .unwrap();
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // targets: ["Universal", "rust", "On-demand", "+ New profile…", "Leave unassigned"]
        // cursor=0 (Universal). Down, Down -> cursor=2 (On-demand).
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);

        // Enter -> assign first queued plugin (eslint@x, alphabetically first) to on_demand.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        assert_eq!(
            v.working_for_test().on_demand,
            vec!["eslint@x".to_string()],
            "eslint@x (first in sorted queue) should land in on_demand"
        );
    }

    // ── Task 3: by-plugin view ───────────────────────────────────────────────

    #[test]
    fn by_plugin_membership_reflects_working() {
        use crate::profile::config::Profiles;
        let w: Profiles = serde_json::from_str(
            r#"{"universal":["serena@x"],"profiles":{"frontend":{"plugins":["eslint@x"],"detect":{}},"node":{"plugins":["eslint@x"],"detect":{}}}}"#,
        )
        .unwrap();
        assert_eq!(
            super::by_plugin::membership(&w, "serena@x"),
            vec!["Universal".to_string()]
        );
        assert_eq!(
            super::by_plugin::membership(&w, "eslint@x"),
            vec!["frontend".to_string(), "node".to_string()]
        );
        assert!(super::by_plugin::membership(&w, "ghost@x").is_empty());
    }

    #[test]
    fn by_plugin_default_view_and_v_toggles() {
        let mut v = view();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        assert!(
            matches!(v.view_for_test(), ViewMode::ByPlugin),
            "new view should default to ByPlugin"
        );
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        assert!(
            matches!(v.view_for_test(), ViewMode::ByProfile),
            "v should toggle to ByProfile"
        );
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        assert!(
            matches!(v.view_for_test(), ViewMode::ByPlugin),
            "v again should toggle back to ByPlugin"
        );
    }

    #[test]
    fn by_plugin_arrows_move_plugin_cursor() {
        let mut v = view_with_two_unassigned(); // inv has 2 plugins
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        assert_eq!(v.plugin_cursor, 0);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        assert_eq!(v.plugin_cursor, 1);
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        assert_eq!(v.plugin_cursor, 0); // wrap
    }

    // ── Task 4: by-plugin multi-membership picker ────────────────────────────

    /// inv has eslint@x; working has two empty profiles "frontend" and "node".
    fn view_with_two_profiles_eslint() -> ProfileView {
        use crate::profile::config::{Profile, Profiles};
        use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "eslint@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![
                SuggestedProfile {
                    name: "frontend".into(),
                    repos: vec![],
                    shared_signals: SharedSignals::default(),
                },
                SuggestedProfile {
                    name: "node".into(),
                    repos: vec![],
                    shared_signals: SharedSignals::default(),
                },
            ],
        };
        let mut working = Profiles::default();
        working
            .profiles
            .insert("frontend".into(), Profile::default());
        working.profiles.insert("node".into(), Profile::default());
        ProfileView::new(inv, working, false, false)
    }

    /// inv has eslint@x; working has "frontend" profile already containing eslint@x.
    fn view_with_eslint_in_frontend() -> ProfileView {
        use crate::profile::config::{Profile, Profiles};
        use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "eslint@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "frontend".into(),
                repos: vec![],
                shared_signals: SharedSignals::default(),
            }],
        };
        let mut working = Profiles::default();
        let mut prof = Profile::default();
        prof.plugins.push("eslint@x".into());
        working.profiles.insert("frontend".into(), prof);
        ProfileView::new(inv, working, false, false)
    }

    #[test]
    fn by_plugin_assign_to_multiple_profiles() {
        let mut v = view_with_two_profiles_eslint();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // cursor on eslint (plugin_cursor=0); ⏎ opens picker
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        assert!(
            v.pick_for_test().is_some(),
            "⏎ on plugin should open the membership picker"
        );

        // picker targets: [Universal, frontend, node, + New profile…]
        // cursor=0 (Universal). Move to frontend (index 1), space.
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // cursor→1 (frontend)
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s); // check frontend
                                                              // Move to node (index 2), space.
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // cursor→2 (node)
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s); // check node
                                                              // ⏎ commit
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.profiles["frontend"]
                .plugins
                .contains(&"eslint@x".to_string()),
            "eslint@x should be in frontend after commit"
        );
        assert!(
            w.profiles["node"].plugins.contains(&"eslint@x".to_string()),
            "eslint@x should be in node after commit"
        );
        assert!(
            !w.universal.contains(&"eslint@x".to_string()),
            "eslint@x should NOT be in universal"
        );
    }

    /// inv has pixijs@x; working has it in on_demand and an empty "frontend" profile.
    fn view_on_demand_plus_frontend() -> ProfileView {
        use crate::profile::config::{Profile, Profiles};
        use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "pixijs@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "frontend".into(),
                repos: vec![],
                shared_signals: SharedSignals::default(),
            }],
        };
        let mut working = Profiles::default();
        working.on_demand.push("pixijs@x".into());
        working
            .profiles
            .insert("frontend".into(), Profile::default());
        ProfileView::new(inv, working, false, false)
    }

    #[test]
    fn picker_commit_removes_reassigned_plugin_from_on_demand() {
        let mut v = view_on_demand_plus_frontend();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // ⏎ opens picker; targets = [Universal, frontend, + New profile…].
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        // cursor 0 = Universal; move to frontend (index 1) and check it.
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s);
        // ⏎ commits.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.profiles["frontend"]
                .plugins
                .contains(&"pixijs@x".to_string()),
            "pixijs@x should be assigned to frontend"
        );
        assert!(
            !w.on_demand.contains(&"pixijs@x".to_string()),
            "pixijs@x must be dropped from on_demand once assigned to a profile \
             (pools stay disjoint)"
        );
    }

    #[test]
    fn picker_commit_with_nothing_checked_keeps_on_demand() {
        let mut v = view_on_demand_plus_frontend();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // Open the picker and commit immediately with nothing checked.
        // membership() returns ["On-demand"], which is not a target, so the
        // picker opens fully unchecked.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.on_demand.contains(&"pixijs@x".to_string()),
            "committing with nothing checked must NOT silently evict from on_demand"
        );
        assert!(
            !w.profiles["frontend"]
                .plugins
                .contains(&"pixijs@x".to_string()),
            "nothing was checked, so no profile assignment"
        );
    }

    #[test]
    fn scan_populates_repos_and_merges_buckets_idempotently() {
        use crate::profile::config::Profiles;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let root = dir.path().display().to_string();
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec![root.clone()]);

        assert!(v.inv_for_test().repos.is_empty(), "starts unscanned");
        v.scan();
        assert_eq!(v.inv_for_test().repos.len(), 1, "scan finds the repo");
        assert!(v.working_for_test().profiles.contains_key("rust"));
        assert_eq!(
            v.working_for_test().scan_roots,
            vec![root.clone()],
            "scan records the scanned set"
        );
        v.scan();
        assert_eq!(v.working_for_test().scan_roots, vec![root]);
        assert_eq!(v.working_for_test().profiles.len(), 1);
    }

    #[test]
    fn scan_is_noop_with_empty_root() {
        use crate::profile::config::Profiles;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false);
        v.scan(); // scan_root defaults to ""
        assert!(v.inv_for_test().repos.is_empty());
        assert!(v.working_for_test().scan_roots.is_empty());
    }

    #[test]
    fn by_plugin_universal_excludes_profiles() {
        // eslint@x is already in "frontend" — open picker, toggle Universal ON,
        // frontend should uncheck, commit → plugin in universal, NOT in frontend.
        let mut v = view_with_eslint_in_frontend();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // ⏎ opens picker; frontend should be pre-checked
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        {
            let pick = v.pick_for_test().expect("picker must be open");
            // targets: [Universal(0), frontend(1), + New profile…(2)]
            // Universal pre-checked? No. frontend pre-checked? Yes.
            assert!(!pick.checked[0], "Universal should NOT be pre-checked");
            assert!(pick.checked[1], "frontend should be pre-checked");
        }

        // cursor is at 0 (Universal). space → Universal ON, frontend OFF.
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s);
        {
            let pick = v.pick_for_test().expect("picker must still be open");
            assert!(pick.checked[0], "Universal should now be checked");
            assert!(
                !pick.checked[1],
                "frontend should be unchecked after mutual exclusivity"
            );
        }

        // ⏎ commit
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.universal.contains(&"eslint@x".to_string()),
            "eslint@x should be in universal after commit"
        );
        assert!(
            !w.profiles["frontend"]
                .plugins
                .contains(&"eslint@x".to_string()),
            "eslint@x should NOT be in frontend after commit"
        );
    }

    #[test]
    fn by_plugin_new_profile_in_picker() {
        let mut v = view_with_two_profiles_eslint();
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // Open picker
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // targets: [Universal(0), frontend(1), node(2), + New profile…(3)]
        // Navigate to "+ New profile…" (index 3 = 3 Downs from 0)
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // → 1
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // → 2
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // → 3

        // space → open naming
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s);
        assert!(
            v.pick_for_test().and_then(|p| p.naming.as_ref()).is_some(),
            "space on '+ New profile…' should open naming"
        );

        // Type "embedded"
        for ch in "embedded".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(ch)), &c, &s);
        }
        // Enter → commit new profile name
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // naming should be cleared, "embedded" added to targets + checked
        assert!(
            v.pick_for_test().is_some_and(|p| p.naming.is_none()),
            "naming should be cleared after Enter"
        );
        assert!(
            v.working_for_test().profiles.contains_key("embedded"),
            "embedded profile should exist in working after naming commit"
        );

        // ⏎ to commit the picker
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        let w = v.working_for_test();
        assert!(
            w.profiles.contains_key("embedded"),
            "embedded profile should persist after picker commit"
        );
        assert!(
            w.profiles["embedded"]
                .plugins
                .contains(&"eslint@x".to_string()),
            "eslint@x should be in embedded after commit"
        );
    }

    #[test]
    fn s_key_triggers_scan_on_board() {
        use crate::profile::config::Profiles;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec![dir.path().display().to_string()]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // Default view is ByPlugin Board, no picker → 's' emits a background
        // Rescan (the walk runs on the job thread, not synchronously here).
        let action = v.on_key(KeyEvent::from(KeyCode::Char('s')), &c, &s);
        assert!(
            matches!(action, Some(Action::Rescan { .. })),
            "'s' must emit a background Rescan, got {action:?}"
        );
        assert!(
            v.inv_for_test().repos.is_empty(),
            "'s' must not walk the filesystem synchronously"
        );
        // Simulate the job thread finishing the walk: the scan finds the repo
        // and suggests the rust profile.
        v.scan();
        assert_eq!(v.inv_for_test().repos.len(), 1, "the scan finds the repo");
        assert!(v.working_for_test().profiles.contains_key("rust"));
    }

    #[test]
    fn by_plugin_header_shows_scan_root_and_hint() {
        use crate::profile::config::Profiles;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec!["/home/u/code".to_string()]);
        let snap = test_support::snap();
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        let mut t = Terminal::new(TestBackend::new(80, 14)).unwrap();
        t.draw(|f| {
            let area = f.area();
            v.render(f, area, &snap, 0, now);
        })
        .unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("All plugins (1)"),
            "header shows plugin count"
        );
        assert!(text.contains("scan:"), "header shows the scan affordance");
        assert!(text.contains("code"), "header shows the scan-root tail");
        assert!(text.contains("s scan"), "header shows the s-scan hint");
    }

    #[test]
    fn by_plugin_list_scrolls_cursor_into_view() {
        // Regression: with more plugins than fit, a cursor at the end must scroll
        // into view. Before the fix the list was a bare Paragraph with no offset,
        // so the lower plugins were unreachable and invisible.
        use crate::profile::config::Profiles;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let plugins: Vec<PluginInfo> = (0..40)
            .map(|i| PluginInfo {
                key: format!("plugin-{i:02}@mkt"),
                scopes: vec![],
                description: None,
            })
            .collect();
        let inv = Inventory {
            plugins,
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false);
        v.plugin_cursor = 39; // select the last plugin

        let snap = test_support::snap();
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        let mut t = Terminal::new(TestBackend::new(80, 14)).unwrap();
        t.draw(|f| {
            let area = f.area();
            v.render(f, area, &snap, 0, now);
        })
        .unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        // Assert on a near-cursor LIST row (plugin-38), not the cursor itself —
        // the description pane always echoes the cursor plugin's key, so checking
        // plugin-39 would pass even if the list never scrolled.
        assert!(
            text.contains("plugin-38"),
            "the list must scroll so rows near the cursor are visible:\n{text}"
        );
        assert!(
            !text.contains("plugin-01"),
            "the top of the list must scroll off when the cursor is at the end"
        );
    }

    #[test]
    fn by_profile_footer_has_scan_hint() {
        let mut v = view(); // helper: ByPlugin default
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s); // → ByProfile
        let hints = v.footer_hints();
        assert!(
            hints.iter().any(|(k, l)| *k == "s" && *l == "scan"),
            "by-profile footer must offer 's scan', got {hints:?}"
        );
    }

    #[test]
    fn scan_walks_the_union_of_multiple_roots() {
        use crate::profile::config::Profiles;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let ra = a.path().join("svc");
        std::fs::create_dir_all(ra.join(".git")).unwrap();
        std::fs::write(ra.join("Cargo.toml"), "[package]").unwrap();
        let rb = b.path().join("web");
        std::fs::create_dir_all(rb.join(".git")).unwrap();
        std::fs::write(rb.join("App.vue"), "x").unwrap();

        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false).with_scan_roots(vec![
            a.path().display().to_string(),
            b.path().display().to_string(),
        ]);
        v.scan();
        assert_eq!(
            v.inv_for_test().repos.len(),
            2,
            "scan should walk the union of both roots"
        );
        assert!(v.working_for_test().profiles.contains_key("rust"));
        assert!(v.working_for_test().profiles.contains_key("frontend"));
        assert_eq!(v.working_for_test().scan_roots.len(), 2);
    }

    #[test]
    fn s_key_emits_rescan_action_and_does_not_scan_on_the_ui_thread() {
        // The depth-6 filesystem walk must move to the job thread: 's' emits an
        // Action the App backgrounds, and must NOT populate repos synchronously.
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec!["/home/u/code".into()]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        let action = v.on_key(KeyEvent::from(KeyCode::Char('s')), &c, &s);
        match action {
            Some(Action::Rescan { roots, .. }) => {
                assert_eq!(roots, vec!["/home/u/code".to_string()])
            }
            other => panic!("'s' must emit Action::Rescan, got {other:?}"),
        }
        assert!(
            v.inv_for_test().repos.is_empty(),
            "'s' must not walk the filesystem on the UI thread"
        );
    }

    #[test]
    fn accept_scan_merges_results_into_the_view() {
        use crate::profile::discover::{RepoSignal, SharedSignals, SuggestedProfile};
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false);
        let outcome = crate::tui::job::ScanOutcome {
            roots: vec!["/x".into()],
            repos: vec![RepoSignal {
                path: "/x/rusty".into(),
                marker_files: vec!["Cargo.toml".into()],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec!["/x/rusty".into()],
                shared_signals: SharedSignals::default(),
            }],
            uncovered: vec![],
            scanned_at: 0,
        };
        v.accept_scan(outcome);
        assert_eq!(v.inv_for_test().repos.len(), 1, "repos must be populated");
        assert!(
            v.working_for_test().profiles.contains_key("rust"),
            "suggested profile must merge into working"
        );
        assert_eq!(
            v.working_for_test().scan_roots,
            vec!["/x".to_string()],
            "scanned roots must be recorded"
        );
    }

    #[test]
    fn apply_scan_assigns_outcome_uncovered_without_walking() {
        // apply_scan must take the uncovered set from the outcome (computed on
        // the job thread) verbatim — never re-derive it from disk. A sentinel
        // path that no filesystem walk could ever produce proves this.
        use crate::profile::discover::RepoSignal;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false);
        let outcome = crate::tui::job::ScanOutcome {
            roots: vec!["/x".into()],
            repos: vec![RepoSignal {
                path: "/x/a".into(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested: vec![],
            uncovered: vec!["SENTINEL-not-from-disk".into()],
            scanned_at: 0,
        };
        v.accept_scan(outcome);
        assert_eq!(
            v.uncovered_for_test(),
            &["SENTINEL-not-from-disk".to_string()],
            "apply_scan must use the outcome's uncovered set, not walk the FS"
        );
    }

    #[test]
    fn apply_scan_clears_uncovered_pending_after_a_full_rescan() {
        // A legacy v1-cache seeds uncovered_pending = true at startup (App::new).
        // A Rescan's outcome is fully decisive (computed from a live walk, no
        // "pending" concept) — apply_scan must clear the flag along with
        // replacing `uncovered`, or Task 7's "…" cue would stay stuck on
        // forever even after a successful rescan resolves it.
        use crate::profile::discover::RepoSignal;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v =
            ProfileView::new(inv, Profiles::default(), false, false).with_uncovered_pending(true);
        assert!(
            v.uncovered_pending_for_test(),
            "seeded pending from a legacy cache"
        );
        let outcome = crate::tui::job::ScanOutcome {
            roots: vec!["/x".into()],
            repos: vec![RepoSignal {
                path: "/x/a".into(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested: vec![],
            uncovered: vec!["/x/a".into()],
            scanned_at: 0,
        };
        v.accept_scan(outcome);
        assert_eq!(
            v.uncovered_for_test(),
            &["/x/a".to_string()],
            "uncovered must be replaced by the rescan outcome"
        );
        assert!(
            !v.uncovered_pending_for_test(),
            "a full rescan is decisive — the pending flag must clear"
        );
    }

    #[test]
    fn with_uncovered_seeds_from_cache_without_walking() {
        // This mirrors what App::new does on startup: seed cached repos + the
        // cached uncovered set via builders. A per-repo WALK over the cached
        // (nonexistent) repo under empty profiles would flag it uncovered; seeding
        // the cached empty set instead must leave uncovered empty — the whole
        // point of keeping startup off the filesystem.
        use crate::profile::discover::RepoSignal;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_repos(vec![RepoSignal {
                path: "/workspace/does-not-exist".into(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }])
            .with_uncovered(vec![]);
        assert_eq!(
            v.inv_for_test().repos.len(),
            1,
            "repos are seeded from the cache"
        );
        assert!(
            v.uncovered_for_test().is_empty(),
            "uncovered must be seeded from the cache (empty), NOT re-walked \
             (a walk would flag the unmatched repo): {:?}",
            v.uncovered_for_test()
        );
    }

    #[test]
    fn dirty_uncovered_reports_only_on_change() {
        use crate::tui::view::View;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // Seeded clean via with_uncovered → no pending write on open.
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_uncovered(vec!["/a".into()]);
        assert!(
            v.dirty_uncovered().is_none(),
            "a freshly seeded uncovered set is clean (no re-persist on open)"
        );
        // A change (e.g. a background recompute landing) must be reported once.
        v.accept_uncovered(vec!["/a".into(), "/b".into()]);
        assert_eq!(
            v.dirty_uncovered(),
            Some(vec!["/a".to_string(), "/b".to_string()]),
            "a changed uncovered set must be offered for persistence"
        );
        assert!(
            v.dirty_uncovered().is_none(),
            "an unchanged uncovered set must not be re-persisted"
        );
    }

    #[test]
    fn by_profile_does_not_claim_r() {
        let mut v = view(); // ByPlugin default
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s); // → ByProfile
        assert!(
            !v.claims_key(KeyCode::Char('r')),
            "by-profile board leaves 'r' to global Refresh"
        );
    }

    #[test]
    fn roots_manager_add_remove_and_commit() {
        use crate::profile::config::Profiles;
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec!["/a".to_string(), "/b".to_string()]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        // by-plugin Board claims 'r'; it opens the modal manager.
        assert!(
            v.claims_key(KeyCode::Char('r')),
            "by-plugin Board claims 'r'"
        );
        v.on_key(KeyEvent::from(KeyCode::Char('r')), &c, &s);

        // Remove the first root (/a). cursor starts at 0.
        v.on_key(KeyEvent::from(KeyCode::Char('d')), &c, &s);

        // Add a new root: press 'a' (seeds <home>/), clear the seed with Home+Deletes,
        // type "/c", Enter.
        v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s);
        // Clear the seeded <home>/ prefix before typing the desired absolute path.
        v.on_key(KeyEvent::from(KeyCode::Home), &c, &s);
        for _ in 0..200 {
            v.on_key(KeyEvent::from(KeyCode::Delete), &c, &s);
        }
        for ch in "/c".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(ch)), &c, &s);
        }
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);

        // Esc closes the manager, committing the list to scan_roots + working.
        v.on_key(KeyEvent::from(KeyCode::Esc), &c, &s);
        assert_eq!(
            v.scan_roots,
            vec!["/b".to_string(), "/c".to_string()],
            "removed /a, kept /b, added /c"
        );
        assert_eq!(
            v.working_for_test().scan_roots,
            vec!["/b".to_string(), "/c".to_string()],
            "close mirrors the edited list into working"
        );
    }

    #[test]
    fn roots_manager_edit_existing_root() {
        use crate::profile::config::Profiles;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec!["/old".to_string()]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        v.on_key(KeyEvent::from(KeyCode::Char('r')), &c, &s); // open manager
                                                              // cursor 0 = "/old". Enter → edit it (pre-filled). Append "er", Enter.
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        for ch in "er".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(ch)), &c, &s);
        }
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Esc), &c, &s); // close
        assert_eq!(v.scan_roots, vec!["/older".to_string()]);
    }

    #[test]
    fn roots_manager_right_accepts_ghost_suggestion() {
        use crate::profile::config::Profiles;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("aardvark")).unwrap();
        let base = dir.path().display().to_string();

        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // A root that is a partial path under the temp dir: "<base>/aa"
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec![format!("{base}/aa")]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        v.on_key(KeyEvent::from(KeyCode::Char('r')), &c, &s); // open manager
        v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s); // ⏎ edit root 0 (pre-filled "<base>/aa")

        // Opening the editor computed a ghost suggestion ("rdvark/").
        {
            let ed = v.roots_editor.as_ref().expect("manager open");
            assert_eq!(ed.suggestion.as_deref(), Some("rdvark/"));
        }

        // → at end accepts the ghost: input becomes "<base>/aardvark/".
        v.on_key(KeyEvent::from(KeyCode::Right), &c, &s);
        let ed = v.roots_editor.as_ref().expect("manager still open");
        assert_eq!(
            ed.input.as_ref().unwrap().value(),
            format!("{base}/aardvark/"),
            "→ should append the suggestion"
        );
    }

    #[test]
    fn ai_draft_defers_scan_to_the_job_thread_when_unscanned() {
        use crate::profile::config::Profiles;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("svc");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "serena@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // claude_available=true so 'a' is honored; offer=false (test 'a' directly).
        let mut v = ProfileView::new(inv, Profiles::default(), true, false)
            .with_scan_roots(vec![dir.path().display().to_string()]);
        let (_h, _d, c) = test_support::ctx();
        let s = test_support::snap();

        let action = v.on_key(KeyEvent::from(KeyCode::Char('a')), &c, &s);

        // The scan is deferred to the background draft job: pressing 'a' must NOT
        // walk the filesystem or merge a bucket synchronously (no UI freeze). The
        // emitted action carries the root so the job can scan before prompting.
        assert!(
            !v.working_for_test().profiles.contains_key("rust"),
            "'a' must not scan/merge synchronously — the job thread does it"
        );
        assert!(
            v.inv_for_test().repos.is_empty(),
            "'a' must not populate repos on the UI thread"
        );
        match action {
            Some(Action::DraftWithClaude { inv, scan_roots }) => {
                assert!(
                    inv.repos.is_empty(),
                    "the inventory scan is deferred to the job thread"
                );
                assert_eq!(
                    scan_roots,
                    vec![dir.path().display().to_string()],
                    "draft must carry the root for the background scan"
                );
            }
            other => panic!("expected DraftWithClaude, got {other:?}"),
        }
    }

    // ── On-demand help overlay (`?` on the On-demand row) ────────────────────

    /// ByProfile board with the cursor parked on the On-demand row (always last).
    fn view_on_demand_row() -> ProfileView {
        let inv = inv_one_plugin();
        let working = crate::profile::draft::scan_draft(&inv, vec![]);
        let mut v = ProfileView::new(inv, working, false, false);
        v.switch_to_by_profile_for_test();
        v.cursor = v.row_labels().len() - 1;
        v
    }

    #[test]
    fn footer_advertises_help_only_on_on_demand_row() {
        let mut v = view_on_demand_row();
        assert!(
            v.footer_hints().iter().any(|(k, _)| *k == "?"),
            "On-demand row selected → footer must advertise ?"
        );
        v.cursor = 0; // Universal
        assert!(
            !v.footer_hints().iter().any(|(k, _)| *k == "?"),
            "other rows must not advertise ?"
        );
    }

    #[test]
    fn question_mark_opens_help_only_on_on_demand_row() {
        let mut v = view_on_demand_row();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('?')), &c, &s);
        assert!(v.on_demand_help, "? on the On-demand row must open help");
        assert_eq!(
            v.footer_hints(),
            vec![("esc", "close")],
            "open overlay → footer shows only esc"
        );
        v.on_key(KeyEvent::from(KeyCode::Esc), &c, &s);
        assert!(!v.on_demand_help, "esc must close help");

        v.cursor = 0;
        v.on_key(KeyEvent::from(KeyCode::Char('?')), &c, &s);
        assert!(!v.on_demand_help, "? elsewhere must not open help");
    }

    #[test]
    fn open_help_swallows_board_keys_and_claims_esc() {
        let mut v = view_on_demand_row();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('?')), &c, &s);

        assert!(v.claims_key(KeyCode::Esc), "overlay must claim Esc");
        assert!(v.claims_key(KeyCode::Tab), "overlay must pause tab cycling");

        // 'v' must not flip the view mode underneath the overlay.
        v.on_key(KeyEvent::from(KeyCode::Char('v')), &c, &s);
        assert!(
            matches!(v.view, ViewMode::ByProfile),
            "v must be swallowed while the overlay is open"
        );
        // 'w' must not open the Apply sub-view underneath the overlay.
        let action = v.on_key(KeyEvent::from(KeyCode::Char('w')), &c, &s);
        assert!(action.is_none(), "w must be swallowed");
        assert!(
            matches!(v.sub, Sub::Board),
            "sub-view must stay Board under the overlay"
        );
        assert!(v.on_demand_help, "overlay stays open on swallowed keys");
    }

    #[test]
    fn open_help_renders_overlay_instead_of_board() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut v = view_on_demand_row();
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();
        v.on_key(KeyEvent::from(KeyCode::Char('?')), &c, &s);

        let mut t = Terminal::new(TestBackend::new(80, 16)).unwrap();
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        t.draw(|f| v.render(f, f.area(), &s, 0, now)).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("/cc-loadout:acquire"),
            "overlay content must replace the board; got: {text}"
        );
    }

    // ── Task 8: detached IndexAtoms job — ProfileView-level state ────────────

    fn working_with_empty_web_profile() -> Profiles {
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            "web".to_string(),
            crate::profile::config::Profile {
                plugins: vec![],
                detect: crate::profile::config::Detect::default(),
            },
        );
        Profiles {
            profiles,
            ..Default::default()
        }
    }

    /// A batch that answered zero repos (e.g. the repo set was empty, or none
    /// of them produced a hit) must still clear both `indexing` flags — the
    /// flag must never wedge just because the answer happened to be empty.
    #[test]
    fn accept_index_clears_indexing_flags_even_with_no_hits() {
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let working = working_with_empty_web_profile();
        let mut v = ProfileView::new(inv.clone(), working.clone(), false, false);
        v.sub = Sub::Detail(Box::new(detail::DetailState::open("web", &inv, &working)));
        v.indexing = true;
        v.indexing_atoms = vec!["glob:*.tsx".to_string()];
        v.index_queue = vec!["glob:*.tsx".to_string()];
        if let Sub::Detail(state) = &mut v.sub {
            state.rules.indexing = true;
        }

        v.accept_index(crate::tui::job::IndexOutcome {
            atoms: vec!["glob:*.tsx".to_string()],
            hits: std::collections::BTreeMap::new(), // no repo answered anything
        });

        assert!(
            !v.indexing,
            "ProfileView.indexing must clear even with empty hits"
        );
        assert!(v.indexing_atoms.is_empty());
        assert!(
            v.index_queue.is_empty(),
            "the answered atom must be dropped from the queue even though no repo hit"
        );
        match &v.sub {
            Sub::Detail(state) => assert!(
                !state.rules.indexing,
                "the open Detail's RulesState.indexing must clear too — it must never wedge"
            ),
            _ => panic!("expected Sub::Detail"),
        }
    }

    /// A rule committed while its atom's index job is still in flight, then
    /// deleted and recommitted before that job lands, must be queued only
    /// ONCE and must NOT spawn a second job — `RulesState` has no memory of
    /// prior pushes, so `ProfileView` (not `RulesState`) owns the dedupe.
    #[test]
    fn dedupe_delete_then_recommit_queues_single_atom_instance() {
        let inv = Inventory {
            plugins: vec![],
            repos: vec![crate::profile::discover::RepoSignal {
                path: "/nonexistent-fake/a".into(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(), // atom never indexed
                override_names: None,
            }],
            suggested_profiles: vec![],
        };
        let working = working_with_empty_web_profile();
        let mut v = ProfileView::new(inv, working, false, false);
        v.switch_to_by_profile_for_test();
        v.cursor = 1; // "web" (row 0 is Universal)

        let (_h, _d, ctx) = test_support::ctx();
        let snap = test_support::snap();

        v.on_key(KeyEvent::from(KeyCode::Enter), &ctx, &snap); // open Detail
        v.on_key(KeyEvent::from(KeyCode::Tab), &ctx, &snap); // focus -> Rules

        // Commit "has any *.tsx".
        v.on_key(KeyEvent::from(KeyCode::Char('a')), &ctx, &snap); // builder (kind-pick)
        v.on_key(KeyEvent::from(KeyCode::Down), &ctx, &snap);
        v.on_key(KeyEvent::from(KeyCode::Down), &ctx, &snap); // -> "has any"
        v.on_key(KeyEvent::from(KeyCode::Enter), &ctx, &snap); // choose "has any"
        for c in "*.tsx".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(c)), &ctx, &snap);
        }
        let action1 = v.on_key(KeyEvent::from(KeyCode::Enter), &ctx, &snap); // commit
        assert!(
            matches!(&action1, Some(Action::IndexAtoms { atoms, .. })
                if atoms == &vec!["glob:*.tsx".to_string()]),
            "first commit must dispatch the atom, got {action1:?}"
        );
        assert!(v.indexing, "a job is now in flight");
        assert!(v.index_queue.is_empty());

        // Delete the rule (cursor followed the committed row).
        v.on_key(KeyEvent::from(KeyCode::Char('d')), &ctx, &snap);

        // Recommit the identical rule while the first job is still in flight.
        v.on_key(KeyEvent::from(KeyCode::Char('a')), &ctx, &snap);
        v.on_key(KeyEvent::from(KeyCode::Down), &ctx, &snap);
        v.on_key(KeyEvent::from(KeyCode::Down), &ctx, &snap);
        v.on_key(KeyEvent::from(KeyCode::Enter), &ctx, &snap);
        for c in "*.tsx".chars() {
            v.on_key(KeyEvent::from(KeyCode::Char(c)), &ctx, &snap);
        }
        let action2 = v.on_key(KeyEvent::from(KeyCode::Enter), &ctx, &snap); // recommit

        assert!(
            action2.is_none(),
            "must NOT dispatch a second job while the first is still in flight, got {action2:?}"
        );
        assert_eq!(
            v.index_queue,
            vec!["glob:*.tsx".to_string()],
            "the requeued atom must appear exactly once, not duplicated"
        );
    }

    /// The by-plugin scan bar's "indexing …" tail names the single atom, or
    /// reports the pattern count for a batch — the same singular/plural split
    /// as the job's toast.
    #[test]
    fn scan_bar_shows_indexing_tail_while_indexing() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let mut v = ProfileView::new(inv, Profiles::default(), false, false)
            .with_scan_roots(vec!["/workspace".into()]);
        let snap = test_support::snap();
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let render = |v: &ProfileView| -> String {
            let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
            t.draw(|f| v.render(f, f.area(), &snap, 0, now)).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        v.indexing = true;
        v.indexing_atoms = vec!["glob:*.tsx".to_string()];
        let text = render(&v);
        assert!(
            text.contains("indexing glob:*.tsx\u{2026}"),
            "single-atom tail: {text}"
        );

        v.indexing_atoms = vec!["glob:*.tsx".into(), "file:go.mod".into()];
        let text2 = render(&v);
        assert!(
            text2.contains("indexing 2 patterns\u{2026}"),
            "batch tail: {text2}"
        );
    }
}
