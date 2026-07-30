use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::profile::config::Profiles;
use crate::tui::theme;
use crate::tui::view::Action;

/// One row in the Apply list: a scanned repo with its matched profiles and
/// the plugins that would be enabled for it.
pub struct ApplyRow {
    pub path: PathBuf,
    pub matched: Vec<String>, // profile names matched for this repo
    pub plugins: Vec<String>, // desired_plugins(working, &matched)
}

/// State for the Apply sub-view.
pub struct ApplyState {
    pub rows: Vec<ApplyRow>,
    pub sel: Vec<bool>, // parallel to rows; true = write this repo
    pub cursor: usize,
    pub expanded: Option<usize>, // row index whose detect reasoning is shown
}

impl ApplyState {
    /// Build the state from the working config and inventory repos.
    pub fn open(repos: &[crate::profile::discover::RepoSignal], working: &Profiles) -> Self {
        let mut rows: Vec<ApplyRow> = Vec::with_capacity(repos.len());
        let mut sel: Vec<bool> = Vec::with_capacity(repos.len());

        for r in repos {
            let path = PathBuf::from(&r.path);
            let matched = crate::profile::detect::detect_profiles(&path, working);
            let plugins = crate::profile::plugins::desired_plugins(working, &matched);
            let checked = !matched.is_empty();
            sel.push(checked);
            rows.push(ApplyRow {
                path,
                matched,
                plugins,
            });
        }

        ApplyState {
            rows,
            sel,
            cursor: 0,
            expanded: None,
        }
    }

    /// Handle a key event. Returns `Some(Action::Commit{..})` on Enter,
    /// `None` for all other keys. Mutates internal state (cursor, sel,
    /// expanded) and sets `sub` back to Board via the caller.
    ///
    /// The caller must check whether a Board-return is requested by
    /// inspecting whether the returned value is `Some` (Enter) or by
    /// checking the separate `should_return` helper — but since Enter
    /// emits an action AND returns to Board, we encode both outcomes in
    /// the return value. For Esc (return to Board, no action) the caller
    /// checks the key code directly.
    pub fn handle_key(&mut self, key: KeyEvent, working: &Profiles) -> ApplyOutcome {
        let n = self.rows.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if n > 0 {
                    self.cursor = (self.cursor + n - 1) % n;
                }
                ApplyOutcome::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if n > 0 {
                    self.cursor = (self.cursor + 1) % n;
                }
                ApplyOutcome::None
            }
            KeyCode::Char(' ') => {
                if n > 0 {
                    self.sel[self.cursor] = !self.sel[self.cursor];
                }
                ApplyOutcome::None
            }
            KeyCode::Char('x') => {
                if n > 0 {
                    self.expanded = match self.expanded {
                        Some(i) if i == self.cursor => None,
                        _ => Some(self.cursor),
                    };
                }
                ApplyOutcome::None
            }
            KeyCode::Enter => {
                let repos: Vec<PathBuf> = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| self.sel[*i])
                    .map(|(_, r)| r.path.clone())
                    .collect();
                ApplyOutcome::Commit(Action::Commit {
                    cfg: working.clone(),
                    repos,
                })
            }
            KeyCode::Esc => ApplyOutcome::Back,
            _ => ApplyOutcome::None,
        }
    }
}

/// Return value from `handle_key` — encodes what the parent mod.rs should do.
pub enum ApplyOutcome {
    None,
    Back,
    Commit(Action),
}

/// Render the Apply sub-view.
pub fn render(state: &ApplyState, working: &Profiles, f: &mut Frame, area: Rect) {
    // Split: header line + body (rest).
    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);

    // Header.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("APPLY — which repos?    ", theme::accent()),
            Span::styled(
                format!(
                    "{} of {} selected",
                    state.sel.iter().filter(|&&s| s).count(),
                    state.sel.len()
                ),
                theme::dim(),
            ),
        ])),
        chunks[0],
    );

    // Body: one line per row (+ optional expand block).
    let mut lines: Vec<Line<'static>> = Vec::new();
    // Line index of the selected row (an expanded row above the cursor inserts
    // extra lines, so the row index is not the line index).
    let mut cursor_line = 0usize;

    for (i, row) in state.rows.iter().enumerate() {
        let checked = state.sel[i];
        let selected = state.cursor == i;
        let check = if checked { "[x]" } else { "[ ]" };
        let path_str = row.path.display().to_string();
        let matched_str = if row.matched.is_empty() {
            "no match".to_string()
        } else {
            row.matched.join(", ")
        };
        let plugins_str = if row.plugins.is_empty() {
            String::new()
        } else {
            format!("+ {}", row.plugins.join(", "))
        };

        let row_style = if selected {
            theme::selection().patch(theme::text())
        } else {
            theme::text()
        };
        let check_style = if selected {
            theme::selection().patch(theme::accent())
        } else {
            theme::accent()
        };
        let dim_style = if selected {
            theme::selection().patch(theme::dim())
        } else {
            theme::dim()
        };

        let mut spans = vec![
            Span::styled(format!("{check} "), check_style),
            Span::styled(format!("{path_str:<40} "), row_style),
            Span::styled(format!("{matched_str:<20} "), dim_style),
        ];
        if !plugins_str.is_empty() {
            spans.push(Span::styled(plugins_str, dim_style));
        }
        if selected {
            cursor_line = lines.len();
        }
        lines.push(Line::from(spans));

        // Expanded reasoning block.
        if state.expanded == Some(i) {
            let explained = crate::profile::detect::detect_profiles_explained(&row.path, working);
            if explained.is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("(no rules matched)", theme::faint()),
                ]));
            } else {
                for (name, reason) in &explained {
                    let val = reason.value.as_deref().unwrap_or("-");
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(format!("{name}: "), theme::faint()),
                        Span::styled(format!("{} = {val}", reason.rule), theme::dim()),
                    ]));
                }
            }
        }
    }

    crate::tui::widgets::render_scrolling_lines(f, chunks[1], lines, cursor_line);
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::{Detect, Profile, Profiles};
    use crate::profile::discover::{
        Inventory, PluginInfo, RepoSignal, SharedSignals, SuggestedProfile,
    };
    use crate::tui::profile::test_support;
    use crate::tui::profile::ProfileView;
    use crate::tui::view::View;
    use ratatui::crossterm::event::KeyCode;
    use std::collections::BTreeMap;

    // ── Test fixtures ─────────────────────────────────────────────────────

    /// Build an `Inventory` with one plugin and one real-fs repo that contains
    /// `Cargo.toml` so `detect_profiles` can match it.
    fn make_inv_and_working() -> (Inventory, Profiles, tempfile::TempDir) {
        let repo_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("Cargo.toml"), "[package]").unwrap();
        let repo_path = repo_dir.path().display().to_string();

        let inv = Inventory {
            plugins: vec![PluginInfo {
                key: "ra@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos: vec![RepoSignal {
                path: repo_path.clone(),
                marker_files: vec!["Cargo.toml".into()],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec!["rs".into()],
                rule_hits: Default::default(),
                override_names: None,
            }],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![repo_path],
                shared_signals: SharedSignals {
                    marker_files: vec!["Cargo.toml".into()],
                    ..Default::default()
                },
            }],
        };

        // Build a "rust" profile that detects Cargo.toml and provides ra@x.
        let mut profiles = BTreeMap::new();
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
        let working = Profiles {
            profiles,
            ..Default::default()
        };

        (inv, working, repo_dir)
    }

    // ── Core tests ────────────────────────────────────────────────────────

    #[test]
    fn apply_lists_repos_with_plugins_and_emits_commit() {
        let (inv, working, _repo_dir) = make_inv_and_working();
        let mut v = ProfileView::new(inv, working, false, false);
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Open Apply (press 'w' on the Board).
        v.on_key(KeyEvent::from(KeyCode::Char('w')), &c, &s);

        // Verify the state was built correctly.
        {
            let state = v
                .apply_state_for_test()
                .expect("Apply should be open after 'w'");
            assert_eq!(state.rows.len(), 1);
            let row = &state.rows[0];
            assert!(
                row.matched.contains(&"rust".to_string()),
                "expected 'rust' in matched, got {:?}",
                row.matched
            );
            assert!(state.sel[0], "matched repo should be default-checked");
            assert!(
                row.plugins.contains(&"ra@x".to_string()),
                "expected 'ra@x' in plugins, got {:?}",
                row.plugins
            );
        }

        // Press Enter → should emit Action::Commit with 1 repo.
        match v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s) {
            Some(crate::tui::view::Action::Commit { repos, .. }) => {
                assert_eq!(repos.len(), 1, "expected exactly 1 repo in commit")
            }
            other => panic!("expected Commit, got {other:?}"),
        }
    }

    #[test]
    fn apply_unchecked_repo_excluded_from_commit() {
        let (inv, working, _repo_dir) = make_inv_and_working();
        let mut v = ProfileView::new(inv, working, false, false);
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        // Open Apply.
        v.on_key(KeyEvent::from(KeyCode::Char('w')), &c, &s);

        // Toggle the (default-checked) matched repo off.
        v.on_key(KeyEvent::from(KeyCode::Char(' ')), &c, &s);

        // Enter → repos should be empty.
        match v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s) {
            Some(crate::tui::view::Action::Commit { repos, .. }) => {
                assert!(
                    repos.is_empty(),
                    "unchecked repo should be excluded; got {repos:?}"
                )
            }
            other => panic!("expected Commit, got {other:?}"),
        }
    }

    #[test]
    fn apply_esc_returns_to_board_no_action() {
        let (inv, working, _repo_dir) = make_inv_and_working();
        let mut v = ProfileView::new(inv, working, false, false);
        let (_home, _data, c) = test_support::ctx();
        let s = test_support::snap();

        v.on_key(KeyEvent::from(KeyCode::Char('w')), &c, &s);
        let result = v.on_key(KeyEvent::from(KeyCode::Esc), &c, &s);
        assert!(result.is_none(), "Esc should return None");

        // After Esc, 'w' should open Apply again (i.e., we're back on the Board).
        v.on_key(KeyEvent::from(KeyCode::Char('w')), &c, &s);
        let state = v.apply_state_for_test();
        assert!(
            state.is_some(),
            "should be back to Apply after re-pressing 'w'"
        );
    }
}
