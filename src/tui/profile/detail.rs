use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::profile::config::Profiles;
use crate::profile::discover::Inventory;
use crate::tui::theme;

/// Which tab has focus in the Detail view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DetailFocus {
    Plugins,
    Rules,
}

/// State for the Detail sub-view — edit a named profile's plugins, detection
/// rules, rename, or delete it.
pub struct DetailState {
    pub name: String,
    pub plugins: crate::tui::multiselect::MultiSelect,
    pub rules: crate::tui::profile::rules::RulesState,
    pub focus: DetailFocus,
    pub renaming: Option<crate::tui::textinput::TextInput>,
    pub explain: Option<crate::tui::profile::explain::ExplainState>,
    /// The profile's detect rules as of open, to tell whether closing this view
    /// actually changed detection (and thus whether `uncovered` must be recomputed).
    original_detect: crate::profile::config::Detect,
    /// Set when this view deleted the profile — a detection change even though
    /// `rules.detect` is untouched.
    deleted: bool,
}

impl DetailState {
    /// Build a DetailState by opening profile `name` from `working`, using
    /// `inv` for the full plugin list and for the rules live preview.
    pub fn open(name: &str, inv: &Inventory, working: &Profiles) -> Self {
        let profile = &working.profiles[name];

        // Plugin MultiSelect: all installed plugin keys; preselect what this
        // profile currently uses.
        let all_plugin_keys: Vec<String> = inv.plugins.iter().map(|p| p.key.clone()).collect();
        let plugins = crate::tui::multiselect::MultiSelect::new(all_plugin_keys, &profile.plugins);

        // Rules tab: open with the profile's current detect rules.
        let rules = crate::tui::profile::rules::RulesState::open(profile.detect.clone(), inv);

        DetailState {
            name: name.to_string(),
            plugins,
            rules,
            focus: DetailFocus::Plugins,
            renaming: None,
            explain: None,
            original_detect: profile.detect.clone(),
            deleted: false,
        }
    }

    /// Whether closing this view changed which repos match a profile — i.e. the
    /// detect rules were edited or the profile was deleted. Editing plugins or
    /// renaming does not affect detection, so callers can skip the (expensive,
    /// all-repos) `uncovered` recompute in those cases.
    pub fn detection_changed(&self) -> bool {
        self.deleted || self.rules.detect != self.original_detect
    }

    /// Mirror the current plugins + detect into `working` under the current name.
    /// The live-save model calls this after every edit, so leaving Detail by any
    /// means (done / Esc / quit) keeps the changes — nothing is silently discarded.
    fn write_back(&self, working: &mut Profiles) {
        working.profiles.insert(
            self.name.clone(),
            crate::profile::config::Profile {
                plugins: self.plugins.selected(),
                detect: self.rules.detect.clone(),
            },
        );
    }

    /// Handle a key event. Returns `true` when we should return to Board.
    /// `working` is mutated on done/delete/rename.
    pub fn handle_key(&mut self, key: KeyEvent, inv: &Inventory, working: &mut Profiles) -> bool {
        if let Some(ti) = &mut self.renaming {
            match key.code {
                KeyCode::Enter => {
                    let new_name = ti.value();
                    let new_name = new_name.trim().to_string();
                    // Accept only: non-empty, different from current, not already a key.
                    if !new_name.is_empty()
                        && new_name != self.name
                        && !working.profiles.contains_key(&new_name)
                    {
                        // Move the existing profile entry under the new key.
                        if let Some(p) = working.profiles.remove(&self.name) {
                            working.profiles.insert(new_name.clone(), p);
                        }
                        self.name = new_name;
                    }
                    self.renaming = None;
                    false
                }
                KeyCode::Esc => {
                    self.renaming = None;
                    false
                }
                _ => {
                    ti.handle_key(key);
                    false
                }
            }
        } else {
            match key.code {
                KeyCode::Tab
                    if self.focus == DetailFocus::Plugins
                        || (self.rules.editor.is_none()
                            && !self.rules.is_picking()
                            && self.explain.is_none()) =>
                {
                    self.focus = match self.focus {
                        DetailFocus::Plugins => DetailFocus::Rules,
                        DetailFocus::Rules => DetailFocus::Plugins,
                    };
                    false
                }
                KeyCode::Char('r') if self.focus == DetailFocus::Plugins => {
                    self.renaming = Some(crate::tui::textinput::TextInput::new(""));
                    false
                }
                KeyCode::Delete if self.focus == DetailFocus::Plugins => {
                    working.profiles.remove(&self.name);
                    self.deleted = true;
                    true // return to Board
                }
                // ── Rules tab ──────────────────────────────────────────────
                _ if self.focus == DetailFocus::Rules => {
                    // Explain overlay owns the keyboard while open.
                    if let Some(ex) = self.explain.as_mut() {
                        let n = ex.repos.len();
                        match key.code {
                            KeyCode::Esc => self.explain = None,
                            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                                ex.cursor = (ex.cursor + 1) % n;
                                ex.report = explain_report_for(
                                    &ex.repos[ex.cursor],
                                    &self.name,
                                    &self.rules.detect,
                                    inv,
                                    working,
                                );
                            }
                            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                                ex.cursor = (ex.cursor + n - 1) % n;
                                ex.report = explain_report_for(
                                    &ex.repos[ex.cursor],
                                    &self.name,
                                    &self.rules.detect,
                                    inv,
                                    working,
                                );
                            }
                            _ => {}
                        }
                        return false;
                    }
                    if self.rules.editor.is_some() || self.rules.is_picking() {
                        // Builder or repo-picker active: give ALL keys to rules.
                        self.rules.handle_key(key, inv);
                        self.write_back(working);
                        false
                    } else {
                        match key.code {
                            KeyCode::Enter => {
                                // Done: changes are already live in `working`.
                                self.write_back(working);
                                true // return to Board
                            }
                            KeyCode::Esc => {
                                // Leave, keeping changes (live-save; no discard).
                                self.write_back(working);
                                true // return to Board
                            }
                            KeyCode::Char('r') => {
                                // 'r' = rename, even from Rules tab (Detail-level action).
                                self.renaming = Some(crate::tui::textinput::TextInput::new(""));
                                false
                            }
                            KeyCode::Char('?') => {
                                // Exclude repos with a .claude/profile override — detect
                                // rules do not classify them, so the per-rule breakdown
                                // would be meaningless. Mirrors the filter in
                                // RulesState::recompute / matching_repos: read straight
                                // off the signal's `override_names`, zero disk I/O.
                                let repos: Vec<String> = inv
                                    .repos
                                    .iter()
                                    .filter(|r| r.override_names.is_none())
                                    .map(|r| r.path.clone())
                                    .collect();
                                if repos.is_empty() {
                                    return false;
                                }
                                let report = explain_report_for(
                                    &repos[0],
                                    &self.name,
                                    &self.rules.detect,
                                    inv,
                                    working,
                                );
                                self.explain = Some(crate::tui::profile::explain::ExplainState {
                                    repos,
                                    cursor: 0,
                                    report,
                                });
                                false
                            }
                            // NOTE: `Delete` is intentionally NOT handled here —
                            // it must fall through to `rules.handle_key`, which
                            // deletes the SELECTED RULE (mirroring `d`). The
                            // profile-deleting `Delete` lives only in the
                            // Plugins-tab arm above. (Bug #3: this arm used to
                            // `working.profiles.remove(&self.name)`, nuking the
                            // whole profile from the Rules tab.)
                            _ => {
                                self.rules.handle_key(key, inv);
                                self.write_back(working);
                                false
                            }
                        }
                    }
                }
                // ── Plugins tab ────────────────────────────────────────────
                KeyCode::Char(' ')
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Char('j')
                | KeyCode::Char('k') => {
                    self.plugins.on_key(key);
                    self.write_back(working);
                    false
                }
                KeyCode::Enter => {
                    // Done from Plugins tab: changes are already live in `working`.
                    self.write_back(working);
                    true
                }
                KeyCode::Esc => {
                    // Leave, keeping changes (live-save; no discard).
                    self.write_back(working);
                    true // return to Board
                }
                _ => false,
            }
        }
    }
}

/// Build an explain report for the repo at `path`, looking its indexed
/// signal up in `inv.repos` — the explain overlay's repo list is itself
/// derived from `inv.repos`, so the signal is always present. Zero disk I/O.
fn explain_report_for(
    path: &str,
    name: &str,
    detect: &crate::profile::config::Detect,
    inv: &Inventory,
    working: &Profiles,
) -> crate::tui::profile::explain::ExplainReport {
    let sig = inv
        .repos
        .iter()
        .find(|r| r.path == path)
        .expect("explain repo list is built from inv.repos");
    crate::tui::profile::explain::explain_repo(sig, name, detect, working)
}

/// Render the Detail sub-view.
///
/// Layout (when not renaming):
///   header_lines tall — profile name + tab bar (3 rows)
///   body           — the focused tab content
///
/// When renaming, the rename prompt occupies the header area. `now_ms` /
/// `scanned_at` are threaded through to the Rules tab's count line, which
/// tags a fully-answerable count with the scan's age.
pub fn render(
    state: &DetailState,
    _inv: &Inventory,
    f: &mut Frame,
    area: Rect,
    now_ms: i64,
    scanned_at: Option<i64>,
) {
    if let Some(ti) = &state.renaming {
        // Rename mode: just the prompt, no tab content.
        let lines: Vec<Line<'static>> = vec![
            Line::from(vec![
                Span::styled("PROFILE  ", theme::accent()),
                Span::styled(state.name.clone(), theme::text()),
            ]),
            Line::raw(""),
            Line::from(vec![
                Span::styled("rename  ", theme::dim()),
                Span::styled(ti.render_line(), theme::text()),
            ]),
        ];
        f.render_widget(Paragraph::new(lines), area);
    } else {
        // Normal mode: split header (3 lines) from body.
        let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);

        // Header block: profile name + tab bar + blank.
        let (plugins_style, rules_style) = match state.focus {
            DetailFocus::Plugins => (theme::accent(), theme::dim()),
            DetailFocus::Rules => (theme::dim(), theme::accent()),
        };
        let header_lines: Vec<Line<'static>> = vec![
            Line::from(vec![
                Span::styled("PROFILE  ", theme::accent()),
                Span::styled(state.name.clone(), theme::text()),
            ]),
            Line::from(vec![
                Span::styled("[Plugins]", plugins_style),
                Span::raw("  "),
                Span::styled("[Rules]", rules_style),
            ]),
            Line::raw(""),
        ];
        f.render_widget(Paragraph::new(header_lines), chunks[0]);

        // Body: the focused tab.
        match state.focus {
            DetailFocus::Plugins => state.plugins.render(f, chunks[1], "Plugins"),
            DetailFocus::Rules => {
                if let Some(ex) = &state.explain {
                    crate::tui::profile::explain::render(ex, f, chunks[1]);
                } else {
                    state.rules.render(f, chunks[1], scanned_at, now_ms);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::discover::{
        Inventory, PluginInfo, RepoSignal, SharedSignals, SuggestedProfile,
    };
    use crate::tui::profile::test_support;
    use crate::tui::view::View;
    use ratatui::crossterm::event::KeyCode;

    /// Build an Inventory with one plugin and one repo that has a Cargo.toml
    /// (so detect can be derived from it).
    fn inv_with_cargo_repo(repo_path: &str) -> Inventory {
        Inventory {
            plugins: vec![PluginInfo {
                key: "ra@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![RepoSignal {
                path: repo_path.to_string(),
                marker_files: vec!["Cargo.toml".into()],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec!["rs".into()],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![repo_path.to_string()],
                shared_signals: SharedSignals {
                    marker_files: vec!["Cargo.toml".into()],
                    ..Default::default()
                },
            }],
        }
    }

    /// Build a working Profiles with a "rust" profile having empty detect and
    /// the given plugins.
    fn working_rust_profile(plugins: Vec<String>) -> Profiles {
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            "rust".to_string(),
            crate::profile::config::Profile {
                plugins,
                detect: crate::profile::config::Detect::default(),
            },
        );
        Profiles {
            profiles,
            ..Default::default()
        }
    }

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    /// Helper to navigate to the "rust" profile row and open Detail.
    fn open_detail_on_rust(inv: Inventory, working: Profiles) -> super::super::ProfileView {
        let mut view = super::super::ProfileView::new(inv, working, false, false);
        // Default is ByPlugin; switch to ByProfile so Enter opens Detail.
        view.switch_to_by_profile_for_test();
        // cursor starts at 0 (Universal); move to 1 (rust).
        view.cursor = 1;
        view
    }

    #[test]
    fn detection_changed_only_when_rules_change_not_plugins() {
        // uncovered depends only on detect rules, so editing plugins must not
        // force the expensive all-repos recompute — this is the 2s freeze the
        // user hit pressing done in the Plugins tab.
        let inv = inv_with_cargo_repo("/tmp/none");
        let working = working_rust_profile(vec![]);
        let mut st = DetailState::open("rust", &inv, &working);
        assert!(
            !st.detection_changed(),
            "a freshly opened detail has no detection change"
        );
        // Toggle a plugin selection — must NOT count as a detection change.
        st.plugins.on_key(k(KeyCode::Char(' ')));
        assert!(
            !st.detection_changed(),
            "editing plugins must not trigger an uncovered recompute"
        );
        // Editing the detect rules IS a detection change.
        st.rules.detect.marker_files.push("Cargo.toml".into());
        assert!(
            st.detection_changed(),
            "editing detect rules is a detection change"
        );
    }

    #[test]
    fn deleting_a_profile_counts_as_detection_change() {
        let inv = inv_with_cargo_repo("/tmp/none");
        let mut working = working_rust_profile(vec![]);
        let mut st = DetailState::open("rust", &inv, &working);
        // Delete is a Plugins-tab action (Detail opens focused on Plugins).
        let done = st.handle_key(k(KeyCode::Delete), &inv, &mut working);
        assert!(done, "Delete returns to the board");
        assert!(
            st.detection_changed(),
            "deleting a profile changes which repos are covered"
        );
        assert!(
            !working.profiles.contains_key("rust"),
            "the profile was removed"
        );
    }

    #[test]
    fn detail_done_after_plugin_only_edit_skips_recompute() {
        // The user's case: toggling plugins then done must NOT spawn the all-repos
        // recompute (which would freeze ~2s) — no Action is emitted.
        let inv = inv_with_cargo_repo("/tmp/none");
        let working = working_rust_profile(vec![]);
        let mut view = open_detail_on_rust(inv, working);
        let (_h, _d, ctx) = test_support::ctx();
        let snap = test_support::snap();
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // open Detail(rust)
        view.on_key(k(KeyCode::Char(' ')), &ctx, &snap); // toggle a plugin
        let action = view.on_key(k(KeyCode::Enter), &ctx, &snap); // done
        assert!(
            action.is_none(),
            "a plugins-only edit must not trigger a background recompute, got {action:?}"
        );
    }

    #[test]
    fn detail_done_after_rule_edit_recomputes_uncovered_synchronously() {
        // Editing detection rules DOES change uncovered, but the recompute now
        // runs inline over the indexed signal (zero I/O) instead of dispatching
        // a background job: `done` emits no Action, and `uncovered` already
        // reflects the new rule by the time `on_key` returns.
        let inv = inv_with_cargo_repo("/workspace/svc");
        let working = working_rust_profile(vec![]);
        let mut view = open_detail_on_rust(inv, working);
        let (_h, _d, ctx) = test_support::ctx();
        let snap = test_support::snap();
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // open Detail(rust)
        assert_eq!(
            view.uncovered_for_test(),
            &["/workspace/svc".to_string()],
            "empty detect rules match nothing yet"
        );
        view.on_key(k(KeyCode::Tab), &ctx, &snap); // focus -> Rules
        view.on_key(k(KeyCode::Char('a')), &ctx, &snap); // open builder (kind pick)
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // choose "path under"
        for c in "/workspace/".chars() {
            view.on_key(k(KeyCode::Char(c)), &ctx, &snap);
        }
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // commit rule
        let action = view.on_key(k(KeyCode::Enter), &ctx, &snap); // done
        assert!(
            action.is_none(),
            "the recompute is inline now — no background Action, got {action:?}"
        );
        assert!(
            view.uncovered_for_test().is_empty(),
            "the new path_prefix rule now matches the repo, coverage updated synchronously"
        );
    }

    #[test]
    fn detail_rules_tab_add_rule_persists_on_done() {
        let inv = inv_with_cargo_repo("/tmp/none");
        let working = working_rust_profile(vec![]);
        let mut view = open_detail_on_rust(inv, working);
        let (_h, _d, ctx) = test_support::ctx();
        let snap = test_support::snap();
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // open Detail
        view.on_key(k(KeyCode::Tab), &ctx, &snap); // focus -> Rules
        view.on_key(k(KeyCode::Char('a')), &ctx, &snap); // open builder (kind pick)
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // choose "path under"
        for c in "/workspace/".chars() {
            view.on_key(k(KeyCode::Char(c)), &ctx, &snap);
        }
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // commit rule
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // done -> write back
        let w = view.working_for_test();
        assert_eq!(
            w.profiles["rust"].detect.path_prefixes,
            vec!["/workspace/".to_string()]
        );
    }

    /// Faithfully translated from the old `detail_toggling_example_repo_rederives_detection`
    /// test. The old test selected a repo in the (now-removed) Repos tab and
    /// expected detect to be re-derived from repo signals via `live_matches` +
    /// `profile_from`. That whole mechanism is gone — detect is now authored
    /// directly in the Rules tab via `RulesState`.
    ///
    /// The new test exercises the equivalent end-to-end path: open Detail on a
    /// profile with an empty detect, switch to the Rules tab, add a "has file"
    /// rule for the marker file that `inv_with_cargo_repo` exposes (Cargo.toml),
    /// press done, and assert the rule is persisted.  This faithfully asserts
    /// the same user intent ("I want this profile to detect Cargo.toml repos")
    /// through the new UI.
    #[test]
    fn detail_rules_tab_add_has_file_rule_persists_on_done() {
        // Build a real temp dir with Cargo.toml so detect_profiles can match.
        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("Cargo.toml"), "[package]").unwrap();
        let repo_path = repo_dir.path().display().to_string();

        let inv = inv_with_cargo_repo(&repo_path);
        // "rust" starts with empty plugins and empty detect.
        let working = working_rust_profile(vec![]);

        let mut view = open_detail_on_rust(inv, working);

        // Open Detail (Enter on profile row).
        let (_home, _data, ctx) = test_support::ctx();
        let snap = test_support::snap();
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        // Switch to Rules tab, add a "has file" rule for Cargo.toml.
        view.on_key(k(KeyCode::Tab), &ctx, &snap); // focus -> Rules
        view.on_key(k(KeyCode::Char('a')), &ctx, &snap); // open builder (kind pick)
        view.on_key(k(KeyCode::Down), &ctx, &snap); // move to "has file" (index 1)
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // choose "has file"
        for c in "Cargo.toml".chars() {
            view.on_key(k(KeyCode::Char(c)), &ctx, &snap);
        }
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // commit rule

        // Enter to done.
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        let w = view.working_for_test();
        assert_eq!(
            w.profiles["rust"].detect.marker_files,
            vec!["Cargo.toml".to_string()],
            "adding a has-file rule for Cargo.toml should persist marker_files detection"
        );
    }

    #[test]
    fn detail_delete_removes_profile_and_unassigns_plugins() {
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "ra@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let working = working_rust_profile(vec!["ra@x".into()]);

        let mut view = open_detail_on_rust(inv, working);
        let (_home, _data, ctx) = test_support::ctx();
        let snap = test_support::snap();

        // Open Detail on "rust".
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        // Press Delete → profile removed, back to Board.
        view.on_key(k(KeyCode::Delete), &ctx, &snap);

        let w = view.working_for_test();
        assert!(
            !w.profiles.contains_key("rust"),
            "delete should remove the rust profile"
        );

        // ra@x should now appear in unassigned_keys.
        let unassigned = crate::profile::draft::unassigned_keys(view.inv_for_test(), w);
        assert!(
            unassigned.contains(&"ra@x".to_string()),
            "ra@x should be unassigned after profile deleted; got {:?}",
            unassigned
        );
    }

    #[test]
    fn detail_rename_moves_profile_key() {
        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "ra@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let working = working_rust_profile(vec!["ra@x".into()]);

        let mut view = open_detail_on_rust(inv, working);
        let (_home, _data, ctx) = test_support::ctx();
        let snap = test_support::snap();

        // Open Detail on "rust".
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        // Press 'r' to enter rename mode.
        view.on_key(k(KeyCode::Char('r')), &ctx, &snap);

        // Type "systems".
        for ch in "systems".chars() {
            view.on_key(k(KeyCode::Char(ch)), &ctx, &snap);
        }

        // Enter to commit rename.
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        let w = view.working_for_test();
        assert!(
            w.profiles.contains_key("systems"),
            "rename should create 'systems' key"
        );
        assert!(
            !w.profiles.contains_key("rust"),
            "rename should remove 'rust' key"
        );
    }

    #[test]
    fn detail_esc_keeps_plugin_edits() {
        let inv = Inventory {
            plugins: vec![
                PluginInfo {
                    key: "ra@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // "rust" starts with ra@x only.
        let working = working_rust_profile(vec!["ra@x".into()]);

        let mut view = open_detail_on_rust(inv, working);
        let (_home, _data, ctx) = test_support::ctx();
        let snap = test_support::snap();

        // Open Detail on "rust".
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        // Plugins focus is default. Cursor starts at 0 (ra@x — already checked).
        // Move down to serena@x (index 1) and toggle it on.
        view.on_key(k(KeyCode::Down), &ctx, &snap);
        view.on_key(k(KeyCode::Char(' ')), &ctx, &snap);

        // Leave via Esc — live-save keeps the edit (no discard).
        view.on_key(k(KeyCode::Esc), &ctx, &snap);

        let w = view.working_for_test();
        assert!(
            w.profiles["rust"].plugins.contains(&"serena@x".to_string()),
            "Esc now KEEPS edits (live-save); got {:?}",
            w.profiles["rust"].plugins
        );
    }

    #[test]
    fn detail_plugin_toggle_is_written_back_immediately() {
        // No "done" Enter: a single toggle must already be in `working`.
        let inv = Inventory {
            plugins: vec![
                PluginInfo {
                    key: "ra@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![],
        };
        let working = working_rust_profile(vec!["ra@x".into()]);
        let mut st = DetailState::open("rust", &inv, &working);
        let mut w = working.clone();
        st.handle_key(k(KeyCode::Down), &inv, &mut w); // -> serena@x
        st.handle_key(k(KeyCode::Char(' ')), &inv, &mut w); // toggle on
        assert!(
            w.profiles["rust"].plugins.contains(&"serena@x".to_string()),
            "toggle must write back to working without a done press"
        );
    }

    #[test]
    fn detail_toggle_plugin_membership_persists_on_done() {
        let inv = Inventory {
            plugins: vec![
                PluginInfo {
                    key: "ra@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // "rust" starts with ra@x only.
        let working = working_rust_profile(vec!["ra@x".into()]);

        let mut view = open_detail_on_rust(inv, working);
        let (_home, _data, ctx) = test_support::ctx();
        let snap = test_support::snap();

        // Open Detail on "rust".
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        // Plugins focus is default. Cursor starts at 0 (ra@x — already checked).
        // Move down to serena@x (index 1) and toggle it on.
        view.on_key(k(KeyCode::Down), &ctx, &snap);
        view.on_key(k(KeyCode::Char(' ')), &ctx, &snap);

        // Enter to commit.
        view.on_key(k(KeyCode::Enter), &ctx, &snap);

        let w = view.working_for_test();
        assert!(
            w.profiles["rust"].plugins.contains(&"serena@x".to_string()),
            "toggled-on serena@x should persist after done; got {:?}",
            w.profiles["rust"].plugins
        );
        assert!(
            w.profiles["rust"].plugins.contains(&"ra@x".to_string()),
            "pre-existing ra@x should still be in plugins"
        );
    }

    /// Regression test: Tab while the explain overlay is open must NOT toggle
    /// the Plugins↔Rules focus. The overlay owns the keyboard; focus must remain
    /// on Rules and the overlay must remain open.
    ///
    /// Without the guard fix (`self.explain.is_none()` added to the Tab arm), the
    /// Tab arm fires unconditionally and toggles focus to Plugins, stealing Tab
    /// from the overlay and breaking keyboard ownership. With the fix, Tab is not
    /// matched by that arm when explain is open, so it falls through to the Rules
    /// branch where it's a no-op (the overlay's `_ => {}` arm).
    #[test]
    fn tab_during_explain_does_not_toggle_focus() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let repo_path = dir.path().display().to_string();

        let inv = inv_with_cargo_repo(&repo_path);
        let working = working_rust_profile(vec![]);

        // Build DetailState directly so we can access .focus and .explain.
        let mut state = DetailState::open("rust", &inv, &working);
        let mut w = working.clone();

        // Focus Rules (Tab from Plugins; explain is None so toggle fires correctly).
        state.handle_key(k(KeyCode::Tab), &inv, &mut w);
        assert_eq!(
            state.focus,
            DetailFocus::Rules,
            "Tab should toggle to Rules"
        );

        // Open explain overlay with '?'.
        state.handle_key(k(KeyCode::Char('?')), &inv, &mut w);
        assert!(state.explain.is_some(), "'?' should open explain overlay");
        assert_eq!(
            state.focus,
            DetailFocus::Rules,
            "focus stays Rules after '?'"
        );

        // Tab while explain is open must NOT toggle focus.
        state.handle_key(k(KeyCode::Tab), &inv, &mut w);
        assert_eq!(
            state.focus,
            DetailFocus::Rules,
            "Tab during explain must NOT toggle focus away from Rules"
        );
        assert!(
            state.explain.is_some(),
            "explain overlay must still be open after Tab"
        );
    }

    /// Regression test: Tab inside a contains-rule builder must reach
    /// `RulesState::handle_key` (switching file→word focus) rather than
    /// toggling the Detail Plugins↔Rules focus.
    ///
    /// Without the guard fix the Tab arm in `DetailState::handle_key` fires
    /// unconditionally, so focus toggles back to Plugins before the builder
    /// can see the Tab.  The word field never receives characters, so
    /// `commit_editor` silently drops the rule (empty word).  The test
    /// therefore fails RED on the original code: `detect.content` is empty.
    /// With the guard (`focus == Plugins || editor.is_none()`) Tab falls
    /// through to the `focus == Rules` branch when a builder is open, reaches
    /// `rules.handle_key`, and correctly switches focus_word — the test
    /// passes GREEN.
    #[test]
    fn detail_contains_rule_tab_switches_to_word_field() {
        let inv = inv_with_cargo_repo("/tmp/none");
        let working = working_rust_profile(vec![]);
        let mut view = open_detail_on_rust(inv, working);
        let (_h, _d, ctx) = test_support::ctx();
        let snap = test_support::snap();

        view.on_key(k(KeyCode::Enter), &ctx, &snap); // open Detail
        view.on_key(k(KeyCode::Tab), &ctx, &snap); // focus -> Rules (no builder open; toggle is OK)
        view.on_key(k(KeyCode::Char('a')), &ctx, &snap); // open builder (kind-pick)
                                                         // Navigate to "contains" (index 3): three Downs.
        view.on_key(k(KeyCode::Down), &ctx, &snap);
        view.on_key(k(KeyCode::Down), &ctx, &snap);
        view.on_key(k(KeyCode::Down), &ctx, &snap);
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // choose "contains" → file input
                                                     // Type the filename.
        for c in "requirements.txt".chars() {
            view.on_key(k(KeyCode::Char(c)), &ctx, &snap);
        }
        // Tab must route to the builder and switch focus to the word field.
        view.on_key(k(KeyCode::Tab), &ctx, &snap);
        // Type the word.
        for c in "torch".chars() {
            view.on_key(k(KeyCode::Char(c)), &ctx, &snap);
        }
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // commit rule
        view.on_key(k(KeyCode::Enter), &ctx, &snap); // done → write back

        let w = view.working_for_test();
        assert_eq!(
            w.profiles["rust"].detect.content,
            vec![crate::profile::config::ContentRule {
                file: "requirements.txt".into(),
                word: "torch".into(),
            }],
            "Tab inside the contains builder must switch to word field; got {:?}",
            w.profiles["rust"].detect.content
        );
    }
}
