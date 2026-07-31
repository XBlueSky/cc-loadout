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
/// the plugins that would be enabled for it. Built entirely from the indexed
/// `RepoSignal` — `open` never touches disk.
pub struct ApplyRow {
    pub sig: crate::profile::discover::RepoSignal,
    pub path: PathBuf,
    pub matched: Vec<String>, // profile names matched for this repo
    pub plugins: Vec<String>, // desired_plugins(working, &matched)
    /// True when at least one profile's detect rules couldn't be fully
    /// answered from the index (a required atom was never scanned) — this
    /// row is an honest "don't know yet", not a decisive match/no-match, and
    /// is never default-checked.
    pub pending: bool,
}

/// State for the Apply sub-view.
pub struct ApplyState {
    pub rows: Vec<ApplyRow>,
    pub sel: Vec<bool>, // parallel to rows; true = write this repo
    pub cursor: usize,
    pub expanded: Option<usize>, // row index whose detect reasoning is shown
}

impl ApplyState {
    /// Build the state from the working config and inventory repos. Zero
    /// disk I/O: every row comes from `signal_detect::detect_from_signal`
    /// evaluating the already-indexed `RepoSignal` against `working`.
    pub fn open(repos: &[crate::profile::discover::RepoSignal], working: &Profiles) -> Self {
        let mut rows: Vec<ApplyRow> = Vec::with_capacity(repos.len());
        let mut sel: Vec<bool> = Vec::with_capacity(repos.len());

        for r in repos {
            let path = PathBuf::from(&r.path);
            let (matched, pending) = crate::profile::signal_detect::detect_from_signal(r, working);
            let plugins = crate::profile::plugins::desired_plugins(working, &matched);
            // A pending row is an unresolved "don't know yet" — never
            // default-checked, so an undecided repo is never silently
            // written without the user looking at it first.
            let checked = !pending && !matched.is_empty();
            sel.push(checked);
            rows.push(ApplyRow {
                sig: r.clone(),
                path,
                matched,
                plugins,
                pending,
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
                let mut repos: Vec<PathBuf> = Vec::new();
                let mut expected: Vec<(PathBuf, Vec<String>)> = Vec::new();
                for (i, row) in self.rows.iter().enumerate() {
                    if self.sel[i] {
                        repos.push(row.path.clone());
                        expected.push((row.path.clone(), row.matched.clone()));
                    }
                }
                ApplyOutcome::Commit(Action::Commit {
                    cfg: working.clone(),
                    repos,
                    expected,
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

/// Render the Apply sub-view. `scanned_at`/`now_ms` feed the dim
/// "preview as of scan {age}" disclaimer — the same scan-age formatter the
/// by-plugin scan bar and Rules tab count line use.
pub fn render(
    state: &ApplyState,
    working: &Profiles,
    scanned_at: Option<i64>,
    now_ms: i64,
    f: &mut Frame,
    area: Rect,
) {
    // Split: title line + preview-age disclaimer + body (rest).
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(area);

    // Title.
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

    // Preview-age disclaimer: this list is built entirely from the index,
    // which can be older than "now" — the write itself always re-detects
    // fresh (commit.rs), so this is a preview, not a promise.
    let age = scanned_at
        .map(|t| super::fmt_age(t, now_ms / 1000))
        .unwrap_or_else(|| "never".to_string());
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("preview as of scan {age} — each repo is re-detected fresh at write time"),
            theme::dim(),
        ))),
        chunks[1],
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
        if row.pending {
            spans.push(Span::styled("pending index  ", dim_style));
        }
        if !plugins_str.is_empty() {
            spans.push(Span::styled(plugins_str, dim_style));
        }
        if selected {
            cursor_line = lines.len();
        }
        lines.push(Line::from(spans));

        // Expanded reasoning block — signal-driven (zero disk I/O), the
        // apply-local counterpart of the Rules tab's index-backed explain.
        if state.expanded == Some(i) {
            let (explained, _pending) =
                crate::profile::signal_detect::detect_from_signal_explained(&row.sig, working);
            if explained.is_empty() {
                let msg = if row.pending {
                    "(pending — some rules not yet indexed)"
                } else {
                    "(no rules matched)"
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(msg, theme::faint()),
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

    crate::tui::widgets::render_scrolling_lines(f, chunks[2], lines, cursor_line);
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
    /// `Cargo.toml`, with `rule_hits` carrying the pre-computed index answer
    /// for it — `ApplyState::open` is index-driven (zero disk I/O), so the
    /// signal's `rule_hits`, not the filesystem, is what makes it match.
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
                rule_hits: [("file:Cargo.toml".to_string(), true)]
                    .into_iter()
                    .collect(),
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
            Some(crate::tui::view::Action::Commit {
                repos, expected, ..
            }) => {
                assert_eq!(repos.len(), 1, "expected exactly 1 repo in commit");
                assert_eq!(expected.len(), 1, "expected 1 preview entry alongside it");
                assert_eq!(expected[0].0, repos[0], "expected/repos paths must line up");
                assert_eq!(
                    expected[0].1,
                    vec!["rust".to_string()],
                    "expected must carry the preview's matched set"
                );
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

    // ── Task 11: index-driven open (zero disk I/O) ───────────────────────

    /// A profile config with a single "rust" profile keyed on `Cargo.toml`,
    /// shared by the hand-built-signal tests below.
    fn rust_working() -> Profiles {
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
        Profiles {
            profiles,
            ..Default::default()
        }
    }

    #[test]
    fn open_decisive_matched_row_is_checked_and_not_pending_with_zero_disk_proof() {
        // Path does not exist on disk — if `open` ever touched disk instead of
        // the index, this row would come back unmatched (or panic).
        let sig = RepoSignal {
            path: "/does/not/exist/matched".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: [("file:Cargo.toml".to_string(), true)]
                .into_iter()
                .collect(),
            override_names: None,
        };
        let working = rust_working();

        let state = ApplyState::open(&[sig], &working);
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].matched, vec!["rust".to_string()]);
        assert!(!state.rows[0].pending, "fully-indexed row is not pending");
        assert!(state.sel[0], "decisive matched row must default-checked");
    }

    #[test]
    fn open_decisive_unmatched_row_is_unchecked_and_not_pending() {
        let sig = RepoSignal {
            path: "/does/not/exist/unmatched".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: [("file:Cargo.toml".to_string(), false)]
                .into_iter()
                .collect(),
            override_names: None,
        };
        let working = rust_working();

        let state = ApplyState::open(&[sig], &working);
        assert!(state.rows[0].matched.is_empty());
        assert!(!state.rows[0].pending, "fully-indexed row is not pending");
        assert!(
            !state.sel[0],
            "decisive unmatched row must default-unchecked"
        );
    }

    #[test]
    fn open_pending_row_is_unchecked_by_default_even_though_never_asked_disk() {
        // rule_hits is empty: the vocabulary's `file:Cargo.toml` atom was
        // never indexed for this repo, so the evaluator can't say match or
        // no-match — it must come back Unknown/pending, and an undecided
        // repo must never be silently checked for write.
        let sig = RepoSignal {
            path: "/does/not/exist/pending".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        };
        let working = rust_working();

        let state = ApplyState::open(&[sig], &working);
        assert!(
            state.rows[0].pending,
            "missing atom must mark the row pending"
        );
        assert!(!state.sel[0], "a pending row must never default-checked");
    }

    fn buffer_text(t: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn render_header_shows_preview_age_and_fresh_write_disclaimer() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let (inv, working, _repo_dir) = make_inv_and_working();
        let state = ApplyState::open(&inv.repos, &working);

        let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
        let scanned_at = 1_700_000_000;
        let now_ms = (scanned_at + 65) * 1000; // just over a minute later
        t.draw(|f| render(&state, &working, Some(scanned_at), now_ms, f, f.area()))
            .unwrap();
        let text = buffer_text(&t);

        assert!(
            text.contains("preview as of scan"),
            "header must show the preview-age line: {text}"
        );
        assert!(
            text.contains("1m ago"),
            "age must come from fmt_age: {text}"
        );
        assert!(
            text.contains("re-detected fresh at write time"),
            "header must disclose that write re-detects: {text}"
        );
    }

    #[test]
    fn render_pending_row_carries_a_dim_pending_annotation() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let sig = RepoSignal {
            path: "/does/not/exist/pending".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        };
        let working = rust_working();
        let state = ApplyState::open(&[sig], &working);

        let mut t = Terminal::new(TestBackend::new(90, 12)).unwrap();
        t.draw(|f| render(&state, &working, None, 0, f, f.area()))
            .unwrap();
        let text = buffer_text(&t);

        assert!(
            text.contains("pending index"),
            "pending row must announce it in the table: {text}"
        );
    }

    #[test]
    fn explain_overlay_on_pending_row_says_pending_not_no_match() {
        // Empty index (nothing answered) → detect_from_signal_explained
        // returns no matches AND pending=true. The overlay must not claim a
        // decisive "no rules matched" for a repo it never actually asked.
        let sig = RepoSignal {
            path: "/does/not/exist/pending".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        };
        let working = rust_working();
        let mut state = ApplyState::open(&[sig], &working);
        state.handle_key(KeyEvent::from(KeyCode::Char('x')), &working);
        assert_eq!(state.expanded, Some(0));

        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 12)).unwrap();
        t.draw(|f| render(&state, &working, None, 0, f, f.area()))
            .unwrap();
        let text = buffer_text(&t);

        assert!(
            !text.contains("no rules matched"),
            "a pending row is not a decisive no-match: {text}"
        );
    }
}
