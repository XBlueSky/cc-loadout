//! Pure logic for the Rules tab: flatten a profile's `Detect` into human
//! rule rows, mutate it, compute live match preview and near-miss hints.
//! No rendering or key handling here (that is the Detail/rules-ui layer).

use crate::profile::config::{ContentRule, Detect, Profile, Profiles};
use crate::profile::discover::{Inventory, RepoSignal};
use crate::tui::textinput::TextInput;
use crate::tui::theme;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// A single detection rule as shown in the Rules tab — a human-readable view
/// over one entry of a profile's `Detect`. `Detect` is the source of truth;
/// rows are derived for display, navigation, and editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRow {
    PathUnder(String),
    HasFile(String),
    HasAny(String),
    Contains {
        file: String,
        word: String,
    },
    /// Read-only legacy rule (from `package_json_deps` / `deps_keywords`),
    /// shown so the user sees the full detection picture.
    Legacy(String),
}

impl RuleRow {
    /// Human label for the left column.
    pub fn label(&self) -> &'static str {
        match self {
            RuleRow::PathUnder(_) => "path under",
            RuleRow::HasFile(_) => "has file",
            RuleRow::HasAny(_) => "has any",
            RuleRow::Contains { .. } => "contains",
            RuleRow::Legacy(_) => "contains (legacy)",
        }
    }

    /// Right-column value text.
    pub fn value(&self) -> String {
        match self {
            RuleRow::PathUnder(s)
            | RuleRow::HasFile(s)
            | RuleRow::HasAny(s)
            | RuleRow::Legacy(s) => s.clone(),
            RuleRow::Contains { file, word } => format!("{file} → {word}"),
        }
    }

    /// Legacy rows are read-only; everything else can be edited/removed freely.
    pub fn editable(&self) -> bool {
        !matches!(self, RuleRow::Legacy(_))
    }
}

/// Flatten a `Detect` into display rows in detection-chain priority order.
pub fn flatten(d: &Detect) -> Vec<RuleRow> {
    let mut rows = Vec::new();
    rows.extend(d.path_prefixes.iter().cloned().map(RuleRow::PathUnder));
    rows.extend(d.marker_files.iter().cloned().map(RuleRow::HasFile));
    rows.extend(d.marker_globs.iter().cloned().map(RuleRow::HasAny));
    rows.extend(d.content.iter().map(|c| RuleRow::Contains {
        file: c.file.clone(),
        word: c.word.clone(),
    }));
    rows.extend(
        d.package_json_deps
            .iter()
            .map(|dep| RuleRow::Legacy(format!("package.json: {dep}"))),
    );
    rows.extend(
        d.deps_keywords
            .iter()
            .map(|kw| RuleRow::Legacy(format!("keyword: {kw}"))),
    );
    rows
}

/// Append a new editable rule to `d`. `Legacy` rows are inert (never added).
pub fn add_rule(d: &mut Detect, row: RuleRow) {
    match row {
        RuleRow::PathUnder(s) => d.path_prefixes.push(s),
        RuleRow::HasFile(s) => d.marker_files.push(s),
        RuleRow::HasAny(s) => d.marker_globs.push(s),
        RuleRow::Contains { file, word } => d.content.push(ContentRule { file, word }),
        RuleRow::Legacy(_) => {}
    }
}

/// Remove the rule shown at display index `i` (must match `flatten` order).
pub fn remove_at(d: &mut Detect, i: usize) {
    let np = d.path_prefixes.len();
    let nf = d.marker_files.len();
    let ng = d.marker_globs.len();
    let nc = d.content.len();
    let npj = d.package_json_deps.len();
    if i < np {
        d.path_prefixes.remove(i);
    } else if i < np + nf {
        d.marker_files.remove(i - np);
    } else if i < np + nf + ng {
        d.marker_globs.remove(i - np - nf);
    } else if i < np + nf + ng + nc {
        d.content.remove(i - np - nf - ng);
    } else if i < np + nf + ng + nc + npj {
        d.package_json_deps.remove(i - np - nf - ng - nc);
    } else {
        let j = i - np - nf - ng - nc - npj;
        debug_assert!(
            j < d.deps_keywords.len(),
            "remove_at index {i} out of range (flatten/remove_at drift)"
        );
        if j < d.deps_keywords.len() {
            d.deps_keywords.remove(j);
        }
    }
}

/// A scanned repo currently matched by a profile's rules, with the rule that fired.
pub struct Match {
    pub path: String,
    #[allow(dead_code)] // reserved for the explain (`?`) command (Plan D)
    pub rule: &'static str,
    pub value: Option<String>,
}

/// A scanned repo that does NOT match, with a one-rule suggestion to catch it.
pub struct NearMiss {
    pub path: String,
    pub suggestion: Option<RuleRow>,
}

/// Repos (from the scanned inventory) currently matched by `detect`, with
/// provenance. Each repo is read from disk (content rules inspect file bodies)
/// via a single-profile probe keyed "_".
pub fn matching_repos(detect: &Detect, repos: &[RepoSignal]) -> Vec<Match> {
    let probe = Profiles {
        profiles: std::collections::BTreeMap::from([(
            "_".to_string(),
            Profile {
                plugins: Vec::new(),
                detect: detect.clone(),
            },
        )]),
        ..Default::default()
    };
    repos
        .iter()
        .filter_map(|r| {
            crate::profile::detect::detect_profiles_explained(std::path::Path::new(&r.path), &probe)
                .into_iter()
                .find(|(name, _)| name == "_")
                .map(|(_, reason)| Match {
                    path: r.path.clone(),
                    rule: reason.rule,
                    value: reason.value,
                })
        })
        .collect()
}

/// Count repos the builder's in-progress rule (on top of the committed rules)
/// would match. Returns None until the in-progress value is non-empty (and, for
/// `contains`, until both file and word are non-empty).
fn scratch_count(detect: &Detect, ed: &RuleEditor, repos: &[RepoSignal]) -> Option<usize> {
    let kind = ed.kind?;
    let file = ed.file.value().trim().to_string();
    if file.is_empty() {
        return None;
    }
    let row = match kind {
        EditorKind::PathUnder => RuleRow::PathUnder(file),
        EditorKind::HasFile => RuleRow::HasFile(file),
        EditorKind::HasAny => RuleRow::HasAny(file),
        EditorKind::Contains => {
            let word = ed
                .word
                .as_ref()
                .map(|w| w.value().trim().to_string())
                .unwrap_or_default();
            if word.is_empty() {
                return None;
            }
            RuleRow::Contains { file, word }
        }
    };
    let mut scratch = detect.clone();
    // When editing an existing rule, remove it from the scratch clone BEFORE
    // adding the in-progress replacement — otherwise the count reflects the
    // union of the old rule and the edited rule (double-counting).
    if let Some(i) = ed.editing {
        remove_at(&mut scratch, i);
    }
    add_rule(&mut scratch, row);
    Some(matching_repos(&scratch, repos).len())
}

/// For each scanned repo NOT in `matched`, suggest one rule (derived from the
/// repo's own signals) that would make it match — or None when the repo
/// exposes no usable signal.
pub fn near_misses(detect: &Detect, repos: &[RepoSignal], matched: &[String]) -> Vec<NearMiss> {
    repos
        .iter()
        .filter(|r| !matched.contains(&r.path))
        .map(|r| NearMiss {
            path: r.path.clone(),
            suggestion: suggest_rule(detect, r),
        })
        .collect()
}

/// The kind of rule being built in the add/edit builder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorKind {
    PathUnder,
    HasFile,
    HasAny,
    Contains,
}

const EDITOR_KINDS: [(EditorKind, &str, &str); 4] = [
    (
        EditorKind::PathUnder,
        "path under",
        "this folder and everything inside",
    ),
    (
        EditorKind::HasFile,
        "has file",
        "a file with this exact name",
    ),
    (
        EditorKind::HasAny,
        "has any",
        "any file matching a glob (*.rs, *.vue)",
    ),
    (
        EditorKind::Contains,
        "contains",
        "a file that contains a word",
    ),
];

/// Add/edit builder: pick a kind, then enter the value(s). `contains` has two
/// inputs (file + word); the others have one. `editing` is Some(row_index) when
/// editing an existing rule (committing replaces it), None when adding.
pub struct RuleEditor {
    kind: Option<EditorKind>,
    pick_cursor: usize,
    file: TextInput,
    word: Option<TextInput>,
    focus_word: bool,
    editing: Option<usize>,
    /// Live count of repos the in-progress rule (committed rules + this one)
    /// would match, recomputed on each value keystroke. `None` until a
    /// non-empty value exists. Spec §4.5.
    live_count: Option<usize>,
    /// Ghost path-completion suffix for a `path under` value (the same
    /// dim-suffix completion the Scan Roots editor offers). `Some` only while
    /// editing a `path under` rule and there is a directory to complete to;
    /// accepted with `→`. Recomputed on each value keystroke.
    suggestion: Option<String>,
}

impl Default for RuleEditor {
    fn default() -> Self {
        RuleEditor {
            kind: None,
            pick_cursor: 0,
            file: TextInput::new(""),
            word: None,
            focus_word: false,
            editing: None,
            live_count: None,
            suggestion: None,
        }
    }
}

/// Stateful editor for one profile's detection rules: the working `Detect`,
/// the selected row, an optional add/edit builder, and cached match/near-miss
/// preview (recomputed whenever the rules change).
pub struct RulesState {
    pub detect: Detect,
    pub cursor: usize,
    pub editor: Option<RuleEditor>,
    repo_pick: Option<RepoPick>,
    matched: Vec<Match>,
    near: Vec<NearMiss>,
    total_repos: usize,
    /// Override-free repos (cached by `recompute`), reused by the builder's
    /// live-count so it does not rebuild the override filter each keystroke.
    live_owned: Vec<RepoSignal>,
}

impl RulesState {
    /// Open the editor for a profile's `Detect`, computing the initial preview.
    pub fn open(detect: Detect, inv: &Inventory) -> Self {
        let mut s = RulesState {
            detect,
            cursor: 0,
            editor: None,
            repo_pick: None,
            matched: Vec::new(),
            near: Vec::new(),
            total_repos: 0,
            live_owned: Vec::new(),
        };
        s.recompute(inv);
        s
    }

    /// Recompute the cached match preview + near-miss list from the current
    /// rules. Repos that carry their own `.claude/profile` override are
    /// EXCLUDED from both lists — detect rules don't classify them, so showing
    /// them as matched or as a near-miss would mislead (Plan B review Minor #1).
    pub fn recompute(&mut self, inv: &Inventory) {
        self.live_owned = inv
            .repos
            .iter()
            .filter(|r| {
                !std::path::Path::new(&r.path)
                    .join(".claude")
                    .join("profile")
                    .is_file()
            })
            .cloned()
            .collect();
        self.total_repos = self.live_owned.len();
        self.matched = matching_repos(&self.detect, &self.live_owned);
        let matched_paths: Vec<String> = self.matched.iter().map(|m| m.path.clone()).collect();
        self.near = near_misses(&self.detect, &self.live_owned, &matched_paths);
    }

    /// True when a builder is open AND past the kind-pick step (i.e. typing a
    /// value), as opposed to the wider `is_building()` (open at all). Retained
    /// to express that distinction in tests; `claims_key` now uses
    /// `is_building()` so the builder owns the keyboard from kind-pick onward.
    #[cfg(test)]
    pub fn editing_text(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.kind.is_some())
    }

    /// True whenever the add/edit builder is open at all — including the
    /// kind-pick step before a value is being typed. The builder owns the
    /// entire keyboard while open (Tab switches fields / is a no-op in
    /// kind-pick, q/r are literal or no-ops, Esc cancels), so `claims_key`
    /// uses this — not the narrower `editing_text()` — to decide claim scope.
    /// Keeping claim-scope == capture-scope is what prevents `App`'s global
    /// shortcuts (q/Esc/Tab) from being applied while the builder is live.
    pub fn is_building(&self) -> bool {
        self.editor.is_some()
    }

    /// True while the builder is on a `path under` value — a filesystem path
    /// with ghost completion, so the footer can advertise `→ complete`.
    pub fn building_path_under(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|e| matches!(e.kind, Some(EditorKind::PathUnder)))
    }

    /// True while the `f` (from repo) picker is open. Like `is_building`, the
    /// picker owns the keyboard (↑↓ move, ⏎ apply, Esc cancel), so `claims_key`
    /// must protect Esc/Tab while it is up.
    pub fn is_picking(&self) -> bool {
        self.repo_pick.is_some()
    }

    fn commit_editor(&mut self, inv: &Inventory) {
        let Some(ed) = self.editor.take() else {
            return;
        };
        let Some(kind) = ed.kind else {
            return;
        };
        let file = ed.file.value().trim().to_string();
        if file.is_empty() {
            return; // empty primary value: drop silently
        }
        let row = match kind {
            EditorKind::PathUnder => RuleRow::PathUnder(file),
            EditorKind::HasFile => RuleRow::HasFile(file),
            EditorKind::HasAny => RuleRow::HasAny(file),
            EditorKind::Contains => {
                let word = ed
                    .word
                    .as_ref()
                    .map(|w| w.value().trim().to_string())
                    .unwrap_or_default();
                if word.is_empty() {
                    return;
                }
                RuleRow::Contains { file, word }
            }
        };
        if let Some(i) = ed.editing {
            remove_at(&mut self.detect, i);
        }
        add_rule(&mut self.detect, row.clone());
        // Cursor-follows-edit: land on the committed rule (last row of its kind).
        let rows = flatten(&self.detect);
        if let Some(idx) = rows.iter().rposition(|r| *r == row) {
            self.cursor = idx;
        }
        self.recompute(inv);
    }

    /// Handle a key — builder-aware. When a builder is active, all keys are
    /// consumed here. Otherwise routes to list navigation + `a`/`e`/`d`.
    pub fn handle_key(&mut self, key: KeyEvent, inv: &Inventory) {
        if let Some(pick) = self.repo_pick.as_mut() {
            let n = pick.repos.len();
            match key.code {
                KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                    pick.cursor = (pick.cursor + 1) % n;
                }
                KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                    pick.cursor = (pick.cursor + n - 1) % n;
                }
                KeyCode::Esc => self.repo_pick = None,
                KeyCode::Enter if pick.cursor < n => {
                    let path = pick.repos[pick.cursor].clone();
                    self.repo_pick = None;
                    if let Some(sig) = inv.repos.iter().find(|r| r.path == path) {
                        let existing = flatten(&self.detect);
                        for row in derive_rules(sig) {
                            if !existing.contains(&row) {
                                add_rule(&mut self.detect, row);
                            }
                        }
                        self.recompute(inv);
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(ed) = self.editor.as_mut() {
            // ── Builder active ────────────────────────────────────────
            if ed.kind.is_none() {
                // Kind-pick sub-mode.
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        ed.pick_cursor = (ed.pick_cursor + 1) % EDITOR_KINDS.len();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        ed.pick_cursor =
                            (ed.pick_cursor + EDITOR_KINDS.len() - 1) % EDITOR_KINDS.len();
                    }
                    KeyCode::Enter => {
                        let k = EDITOR_KINDS[ed.pick_cursor].0;
                        ed.kind = Some(k);
                        if k == EditorKind::Contains {
                            ed.word = Some(TextInput::new(""));
                        }
                    }
                    KeyCode::Esc => {
                        self.editor = None;
                    }
                    _ => {}
                }
            } else {
                // Value-entry sub-mode.
                match key.code {
                    KeyCode::Enter => {
                        self.commit_editor(inv);
                        return;
                    }
                    KeyCode::Esc => {
                        self.editor = None;
                        return;
                    }
                    KeyCode::Tab if ed.word.is_some() => ed.focus_word = !ed.focus_word,
                    // `→` accepts the ghost path-completion for a `path under`
                    // value (only at end-of-input, only when there is a
                    // completion) — mirrors the Scan Roots editor.
                    KeyCode::Right
                        if matches!(ed.kind, Some(EditorKind::PathUnder))
                            && ed.file.at_end()
                            && ed.suggestion.is_some() =>
                    {
                        let suff = ed.suggestion.take().unwrap();
                        let val = ed.file.value() + &suff;
                        ed.file = TextInput::new(&val);
                    }
                    _ => {
                        if ed.focus_word {
                            if let Some(w) = ed.word.as_mut() {
                                w.handle_key(key);
                            }
                        } else {
                            ed.file.handle_key(key);
                        }
                    }
                }
                // Refresh the live count from the committed rules + this
                // in-progress rule. Cheap COUNT only (no near-miss); reuses the
                // cached override-free repo set. (Avoids re-running the full
                // preview per keystroke — see Plan C c050de4.)
                ed.live_count = scratch_count(&self.detect, ed, &self.live_owned);
                // Refresh the ghost path-completion for a `path under` value.
                ed.suggestion = if matches!(ed.kind, Some(EditorKind::PathUnder)) {
                    super::by_plugin::dir_suggestion(&ed.file.value())
                } else {
                    None
                };
            }
            return;
        }
        // ── List focused, no builder ─────────────────────────────────
        // Bind the flattened rows once: it is consulted for `n`, for the `e`
        // editability guard and the editor seed, and for the `d`/Delete guard.
        let rows = flatten(&self.detect);
        let n = rows.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if n > 0 => {
                self.cursor = (self.cursor + 1) % n;
            }
            KeyCode::Up | KeyCode::Char('k') if n > 0 => {
                self.cursor = (self.cursor + n - 1) % n;
            }
            KeyCode::Char('a') => self.editor = Some(RuleEditor::default()),
            KeyCode::Char('e') if self.cursor < n && rows[self.cursor].editable() => {
                self.editor = Some(editor_for(&rows[self.cursor], self.cursor));
            }
            // Legacy rows are read-only — guarded by `.editable()` exactly like
            // the `e` arm, so `d`/Delete cannot remove a row the UI presents as
            // read-only (Minor #8).
            KeyCode::Char('d') | KeyCode::Delete
                if self.cursor < n && rows[self.cursor].editable() =>
            {
                remove_at(&mut self.detect, self.cursor);
                let new_n = flatten(&self.detect).len();
                self.cursor = if new_n == 0 {
                    0
                } else {
                    self.cursor.min(new_n - 1)
                };
                self.recompute(inv);
            }
            KeyCode::Char('f') if !inv.repos.is_empty() => {
                self.repo_pick = Some(RepoPick {
                    repos: inv.repos.iter().map(|r| r.path.clone()).collect(),
                    cursor: 0,
                });
            }
            _ => {}
        }
    }

    /// Render the Rules tab body: if a builder is open draw the builder overlay;
    /// otherwise draw the rule list, match-count line, and near-miss panel.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        if let Some(pick) = &self.repo_pick {
            let mut lines: Vec<Line<'static>> = vec![
                Line::from(Span::styled(
                    "Prefill rules from a repo — pick one",
                    theme::accent(),
                )),
                Line::raw(""),
            ];
            let cursor_line = lines.len() + pick.cursor; // title + blank precede row 0
            for (i, p) in pick.repos.iter().enumerate() {
                let marker = if i == pick.cursor { "▸ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(marker.to_string(), theme::accent()),
                    Span::styled(short_path(p).to_string(), theme::text()),
                ]));
            }
            crate::tui::widgets::render_scrolling_lines(f, area, lines, cursor_line);
            return;
        }
        if let Some(ed) = &self.editor {
            render_editor(ed, f, area);
            return;
        }

        let rows = flatten(&self.detect);
        // The rule list wants one line per rule + a heading, but cap it so a long
        // list can't crowd out the match-count preview below — the list scrolls
        // within its region instead of squeezing the preview to nothing.
        let list_h = (rows.len().max(1) as u16 + 1).min(area.height.saturating_sub(4).max(1));
        let chunks = Layout::vertical([Constraint::Length(list_h), Constraint::Min(0)]).split(area);

        // ── Rule list ───────────────────────────────────────────────
        let mut lines: Vec<Line<'static>> = Vec::new();
        if rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "No rules yet — press a to add one",
                theme::faint(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Matches a repo when any of these holds",
                theme::dim(),
            )));
            for (i, row) in rows.iter().enumerate() {
                let marker = if i == self.cursor { "▸ " } else { "  " };
                let label_style = if row.editable() {
                    theme::dim()
                } else {
                    theme::faint()
                };
                lines.push(Line::from(vec![
                    Span::styled(marker.to_string(), theme::accent()),
                    Span::styled(format!("{:<14}", row.label()), label_style),
                    Span::styled(row.value(), theme::text()),
                ]));
            }
        }
        // +1: a heading line precedes the rules, so row `cursor` is at line cursor+1.
        let cursor_line = if rows.is_empty() { 0 } else { self.cursor + 1 };
        crate::tui::widgets::render_scrolling_lines(f, chunks[0], lines, cursor_line);

        // ── Match count + near-miss panel ───────────────────────────
        let mut preview: Vec<Line<'static>> = Vec::new();
        preview.push(Line::from(Span::styled(
            format!(
                "● {} of {} scanned repos match",
                self.matched.len(),
                self.total_repos
            ),
            theme::accent(),
        )));
        for m in &self.matched {
            let why = m.value.clone().unwrap_or_default();
            preview.push(Line::from(Span::styled(
                format!("   {}   {}", short_path(&m.path), why),
                theme::text(),
            )));
        }
        let suggestable: Vec<&NearMiss> = self
            .near
            .iter()
            .filter(|n| n.suggestion.is_some())
            .collect();
        if !suggestable.is_empty() {
            preview.push(Line::raw(""));
            preview.push(Line::from(Span::styled(
                format!("╴ {} nearby, not matched", suggestable.len()),
                theme::faint(),
            )));
            for n in suggestable {
                let hint = match &n.suggestion {
                    Some(RuleRow::HasAny(g)) => format!("add  has any {g}?"),
                    Some(RuleRow::HasFile(file)) => format!("add  has file {file}?"),
                    Some(RuleRow::Contains { file, word }) => {
                        format!("add  contains {file} → {word}?")
                    }
                    Some(RuleRow::PathUnder(p)) => format!("add  path under {p}?"),
                    Some(RuleRow::Legacy(_)) | None => String::new(),
                };
                preview.push(Line::from(Span::styled(
                    format!("   {}   {}", short_path(&n.path), hint),
                    theme::faint(),
                )));
            }
        }
        f.render_widget(Paragraph::new(preview), chunks[1]);
    }
}

/// Build an editor pre-filled to edit the existing rule at `index`.
fn editor_for(row: &RuleRow, index: usize) -> RuleEditor {
    match row {
        RuleRow::PathUnder(s) => RuleEditor {
            kind: Some(EditorKind::PathUnder),
            file: TextInput::new(s),
            suggestion: super::by_plugin::dir_suggestion(s),
            editing: Some(index),
            ..Default::default()
        },
        RuleRow::HasFile(s) => RuleEditor {
            kind: Some(EditorKind::HasFile),
            file: TextInput::new(s),
            editing: Some(index),
            ..Default::default()
        },
        RuleRow::HasAny(s) => RuleEditor {
            kind: Some(EditorKind::HasAny),
            file: TextInput::new(s),
            editing: Some(index),
            ..Default::default()
        },
        RuleRow::Contains { file, word } => RuleEditor {
            kind: Some(EditorKind::Contains),
            file: TextInput::new(file),
            word: Some(TextInput::new(word)),
            editing: Some(index),
            ..Default::default()
        },
        RuleRow::Legacy(_) => RuleEditor::default(), // not reachable: callers guard on editable()
    }
}

/// Render the builder overlay (borderless): kind-pick or value-entry.
fn render_editor(ed: &RuleEditor, f: &mut Frame, area: Rect) {
    // "edit rule" when replacing an existing row, "add rule" when appending.
    let verb = if ed.editing.is_some() { "edit" } else { "add" };
    let mut lines: Vec<Line<'static>> = Vec::new();
    match ed.kind {
        None => {
            lines.push(Line::from(Span::styled(
                format!("{verb} rule — pick a kind"),
                theme::accent(),
            )));
            lines.push(Line::raw(""));
            for (i, (_, label, hint)) in EDITOR_KINDS.iter().enumerate() {
                let marker = if i == ed.pick_cursor { "▸ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(marker.to_string(), theme::accent()),
                    Span::styled(format!("{label:<12}"), theme::text()),
                    Span::styled((*hint).to_string(), theme::faint()),
                ]));
            }
        }
        Some(kind) => {
            let title = EDITOR_KINDS
                .iter()
                .find(|(k, _, _)| *k == kind)
                .map(|(_, l, _)| *l)
                .unwrap_or("");
            lines.push(Line::from(Span::styled(
                format!("{verb} rule · {title}"),
                theme::accent(),
            )));
            lines.push(Line::raw(""));
            if let Some(w) = &ed.word {
                let fmark = if ed.focus_word { "  " } else { "▸ " };
                let wmark = if ed.focus_word { "▸ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(format!("{fmark}file  "), theme::dim()),
                    Span::styled(ed.file.render_line(), theme::text()),
                ]));
                lines.push(Line::from(vec![
                    Span::styled(format!("{wmark}word  "), theme::dim()),
                    Span::styled(w.render_line(), theme::text()),
                ]));
            } else {
                let mut spans = vec![
                    Span::styled("▸ ", theme::accent()),
                    Span::styled(ed.file.render_line(), theme::text()),
                ];
                // Ghost path-completion (dim suffix) for a `path under` value.
                if let Some(sug) = &ed.suggestion {
                    spans.push(Span::styled(sug.clone(), theme::faint()));
                }
                lines.push(Line::from(spans));
            }
            if let Some(n) = ed.live_count {
                lines.push(Line::raw(""));
                lines.push(Line::from(Span::styled(
                    format!("● matches {n} repos"),
                    theme::accent(),
                )));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// Last path component for compact display (full path is unwieldy in the panel).
fn short_path(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// All rules derivable from one repo's signals, in detection-chain order:
/// marker files → globs → package.json deps (as content rules). Used by the
/// `f` (from repo) picker to prefill the rule list; the user edits afterward.
fn derive_rules(r: &RepoSignal) -> Vec<RuleRow> {
    let mut rows = Vec::new();
    rows.extend(r.marker_files.iter().cloned().map(RuleRow::HasFile));
    rows.extend(r.marker_globs.iter().cloned().map(RuleRow::HasAny));
    rows.extend(r.package_json_deps.iter().map(|dep| RuleRow::Contains {
        file: "package.json".to_string(),
        word: dep.clone(),
    }));
    rows
}

/// The `f` (from repo) picker: choose one scanned repo to prefill rules from.
struct RepoPick {
    repos: Vec<String>,
    cursor: usize,
}

/// Suggest the single most useful rule to add so `r` would match: prefer a glob
/// the repo has but the profile lacks, then an uncovered marker file, then a
/// package.json dep as a content rule.
fn suggest_rule(detect: &Detect, r: &RepoSignal) -> Option<RuleRow> {
    if let Some(g) = r
        .marker_globs
        .iter()
        .find(|g| !detect.marker_globs.contains(g))
    {
        return Some(RuleRow::HasAny(g.clone()));
    }
    if let Some(f) = r
        .marker_files
        .iter()
        .find(|f| !detect.marker_files.contains(f) && !detect.content.iter().any(|c| c.file == **f))
    {
        return Some(RuleRow::HasFile(f.clone()));
    }
    if let Some(dep) = r.package_json_deps.first() {
        return Some(RuleRow::Contains {
            file: "package.json".to_string(),
            word: dep.clone(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::discover::{Inventory, PluginInfo, RepoSignal};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use ratatui::Terminal;

    fn inv_with_repos(repos: Vec<crate::profile::discover::RepoSignal>) -> Inventory {
        Inventory {
            plugins: vec![PluginInfo {
                key: "p@x".into(),
                scopes: vec![],
                description: None,
            }],
            repos,
            suggested_profiles: vec![],
        }
    }

    #[test]
    fn recompute_counts_matches_over_total() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let path = dir.path().display().to_string();
        let inv = inv_with_repos(vec![
            repo(&path, &["Cargo.toml"], &[], &[]),
            repo("/nonexistent-xyz", &[], &[], &[]),
        ]);
        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into());
        let st = RulesState::open(d, &inv);
        assert_eq!(st.matched.len(), 1, "only the real Cargo.toml repo matches");
        assert_eq!(st.total_repos, 2);
    }

    #[test]
    fn render_shows_rules_and_match_count() {
        let inv = inv_with_repos(vec![repo("/a", &[], &["*.vue"], &[])]);
        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into());
        let st = RulesState::open(d, &inv);
        let mut t = Terminal::new(TestBackend::new(70, 20)).unwrap();
        t.draw(|f| st.render(f, f.area())).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("has file"), "rule label shown: {text}");
        assert!(text.contains("Cargo.toml"), "rule value shown");
        assert!(text.contains("match"), "match-count line shown");
    }

    fn repo(path: &str, files: &[&str], globs: &[&str], deps: &[&str]) -> RepoSignal {
        RepoSignal {
            path: path.into(),
            marker_files: files.iter().map(|s| s.to_string()).collect(),
            marker_globs: globs.iter().map(|s| s.to_string()).collect(),
            package_json_deps: deps.iter().map(|s| s.to_string()).collect(),
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        }
    }

    #[test]
    fn matching_repos_reports_path_and_provenance() {
        // Real temp repo with Cargo.toml so detect_profiles_explained can match on disk.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let path = dir.path().display().to_string();
        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into());

        let repos = vec![repo(&path, &["Cargo.toml"], &[], &[])];
        let got = matching_repos(&d, &repos);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, path);
        assert_eq!(got[0].rule, "marker_file");
        assert_eq!(got[0].value.as_deref(), Some("Cargo.toml"));
    }

    #[test]
    fn near_misses_suggest_a_rule_from_the_repos_own_signals() {
        let d = Detect::default(); // matches nothing
        let repos = vec![
            repo("/a", &[], &["*.vue"], &[]),  // -> suggest HasAny(*.vue)
            repo("/b", &["go.mod"], &[], &[]), // -> suggest HasFile(go.mod)
            repo("/c", &[], &[], &["react"]),  // -> suggest Contains(package.json, react)
            repo("/d", &[], &[], &[]),         // -> no signal, suggestion None
        ];
        let nm = near_misses(&d, &repos, &[]);
        assert_eq!(nm.len(), 4);
        assert_eq!(nm[0].suggestion, Some(RuleRow::HasAny("*.vue".into())));
        assert_eq!(nm[1].suggestion, Some(RuleRow::HasFile("go.mod".into())));
        assert_eq!(
            nm[2].suggestion,
            Some(RuleRow::Contains {
                file: "package.json".into(),
                word: "react".into()
            })
        );
        assert_eq!(nm[3].suggestion, None);
    }

    fn detect() -> Detect {
        Detect {
            path_prefixes: vec!["/workspace/".into()],
            marker_files: vec!["Cargo.toml".into()],
            marker_globs: vec!["*.vue".into()],
            content: vec![ContentRule {
                file: "requirements.txt".into(),
                word: "torch".into(),
            }],
            package_json_deps: vec!["svelte".into()],
            deps_keywords: vec!["openai".into()],
        }
    }

    #[test]
    fn flatten_orders_by_detection_chain_and_marks_legacy() {
        let rows = flatten(&detect());
        assert_eq!(
            rows,
            vec![
                RuleRow::PathUnder("/workspace/".into()),
                RuleRow::HasFile("Cargo.toml".into()),
                RuleRow::HasAny("*.vue".into()),
                RuleRow::Contains {
                    file: "requirements.txt".into(),
                    word: "torch".into()
                },
                RuleRow::Legacy("package.json: svelte".into()),
                RuleRow::Legacy("keyword: openai".into()),
            ]
        );
        assert!(rows[3].editable());
        assert!(!rows[4].editable(), "legacy rows are read-only");
        assert_eq!(rows[3].label(), "contains");
        assert_eq!(rows[3].value(), "requirements.txt → torch");
        assert_eq!(rows[0].label(), "path under");
    }

    #[test]
    fn add_rule_appends_to_the_right_field() {
        let mut d = Detect::default();
        add_rule(&mut d, RuleRow::PathUnder("/x/".into()));
        add_rule(&mut d, RuleRow::HasFile("go.mod".into()));
        add_rule(&mut d, RuleRow::HasAny("*.rs".into()));
        add_rule(
            &mut d,
            RuleRow::Contains {
                file: "package.json".into(),
                word: "react".into(),
            },
        );
        add_rule(&mut d, RuleRow::Legacy("ignored".into())); // legacy never added
        assert_eq!(d.path_prefixes, vec!["/x/".to_string()]);
        assert_eq!(d.marker_files, vec!["go.mod".to_string()]);
        assert_eq!(d.marker_globs, vec!["*.rs".to_string()]);
        assert_eq!(
            d.content,
            vec![ContentRule {
                file: "package.json".into(),
                word: "react".into()
            }]
        );
        assert!(d.package_json_deps.is_empty() && d.deps_keywords.is_empty());
    }

    fn detect_three() -> Detect {
        let mut d = Detect::default();
        d.path_prefixes.push("/x/".into());
        d.marker_files.push("Cargo.toml".into());
        d.marker_globs.push("*.vue".into());
        d
    }

    #[test]
    fn down_moves_cursor_and_wraps() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(detect_three(), &inv); // 3 rows
        assert_eq!(st.cursor, 0);
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv);
        assert_eq!(st.cursor, 1);
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv);
        assert_eq!(st.cursor, 0, "wraps past the last row");
    }

    /// A `path under` value offers the same ghost directory-completion the Scan
    /// Roots editor does: typing a partial path shows a dim suffix, and `→`
    /// accepts it. (The original report: "path under has no suggestion".)
    #[test]
    fn path_under_builder_offers_ghost_completion() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("alpha")).unwrap();
        let base_str = base.path().display().to_string();

        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv); // builder (kind-pick)
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose "path under" (kind 0)

        // Type "<base>/al" — should ghost-complete "alpha".
        for c in format!("{base_str}/al").chars() {
            st.handle_key(key(c), &inv);
        }
        assert_eq!(
            st.editor.as_ref().unwrap().suggestion.as_deref(),
            Some("pha/"),
            "ghost completion suggests the rest of 'alpha/'"
        );

        // `→` accepts the completion, extending the value.
        st.handle_key(KeyEvent::from(KeyCode::Right), &inv);
        assert_eq!(
            st.editor.as_ref().unwrap().file.value(),
            format!("{base_str}/alpha/"),
            "→ appends the ghost suffix"
        );
    }

    /// Non-path kinds (e.g. `has file`) never carry a ghost path completion.
    #[test]
    fn has_file_builder_has_no_ghost_completion() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv); // builder
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv); // -> has file
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose "has file"
        for c in "Cargo".chars() {
            st.handle_key(key(c), &inv);
        }
        assert!(
            st.editor.as_ref().unwrap().suggestion.is_none(),
            "has file is a repo-root basename, not a path — no ghost completion"
        );
    }

    #[test]
    fn d_deletes_selected_rule_and_clamps_cursor() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(detect_three(), &inv);
        st.cursor = 2; // *.vue
        st.handle_key(KeyEvent::from(KeyCode::Char('d')), &inv);
        assert!(st.detect.marker_globs.is_empty(), "glob rule removed");
        assert_eq!(flatten(&st.detect).len(), 2);
        assert_eq!(st.cursor, 1, "cursor clamped to last valid row");
    }

    /// Minor #8: `d`/Delete on a legacy (read-only) row must be a no-op, mirroring
    /// the `e` arm's `.editable()` guard. `detect()` rows 4 & 5 are Legacy.
    #[test]
    fn d_and_delete_do_not_remove_legacy_rows() {
        let inv = inv_with_repos(vec![]);
        let before = flatten(&detect());
        assert!(!before[4].editable(), "row 4 is a legacy (read-only) row");

        // 'd' on a legacy row: nothing removed.
        let mut st = RulesState::open(detect(), &inv);
        st.cursor = 4; // package.json: svelte (legacy)
        st.handle_key(KeyEvent::from(KeyCode::Char('d')), &inv);
        assert_eq!(
            flatten(&st.detect),
            before,
            "'d' on a legacy row must not remove anything"
        );

        // Delete on a legacy row: also nothing removed.
        let mut st2 = RulesState::open(detect(), &inv);
        st2.cursor = 5; // keyword: openai (legacy)
        st2.handle_key(KeyEvent::from(KeyCode::Delete), &inv);
        assert_eq!(
            flatten(&st2.detect),
            before,
            "Delete on a legacy row must not remove anything"
        );
    }

    /// Minor #11 corollary: `is_building()` is true the moment the builder opens
    /// (kind-pick), before any value is typed — wider than `editing_text()`.
    #[test]
    fn is_building_true_at_kind_pick_before_editing_text() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        assert!(!st.is_building(), "no builder open yet");
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv); // open builder (kind-pick)
        assert!(st.is_building(), "is_building true at the kind-pick step");
        assert!(
            !st.editing_text(),
            "editing_text still false until a kind is chosen"
        );
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose kind → value entry
        assert!(
            st.is_building() && st.editing_text(),
            "both true once typing"
        );
    }

    /// Minor #9: the builder title reads "edit rule" when replacing an existing
    /// row (`editing.is_some()`) and "add rule" when appending.
    #[test]
    fn render_editor_title_reflects_edit_vs_add() {
        let render = |ed: &RuleEditor| -> String {
            let mut t = Terminal::new(TestBackend::new(60, 8)).unwrap();
            t.draw(|f| render_editor(ed, f, f.area())).unwrap();
            t.backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect()
        };

        // Editing an existing path-under rule (index 0).
        let editing = editor_for(&RuleRow::PathUnder("/x/".into()), 0);
        let txt = render(&editing);
        assert!(
            txt.contains("edit rule"),
            "editing shows 'edit rule': {txt}"
        );
        assert!(!txt.contains("add rule"), "editing must not say 'add rule'");

        // A fresh builder (adding) shows "add rule".
        let adding = RuleEditor::default();
        let txt2 = render(&adding);
        assert!(txt2.contains("add rule"), "adding shows 'add rule': {txt2}");
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    #[test]
    fn derive_rules_covers_all_repo_signals() {
        let r = repo(
            "/x",
            &["Cargo.toml", "go.mod"],
            &["*.vue"],
            &["react", "svelte"],
        );
        let rows = derive_rules(&r);
        assert_eq!(
            rows,
            vec![
                RuleRow::HasFile("Cargo.toml".into()),
                RuleRow::HasFile("go.mod".into()),
                RuleRow::HasAny("*.vue".into()),
                RuleRow::Contains {
                    file: "package.json".into(),
                    word: "react".into()
                },
                RuleRow::Contains {
                    file: "package.json".into(),
                    word: "svelte".into()
                },
            ]
        );
    }

    /// M3 regression: editing an existing rule must not double-count.
    ///
    /// Setup:
    ///   - detect: marker_files = ["Cargo.toml"]
    ///   - repo_a: has Cargo.toml  → matched by the original rule
    ///   - repo_b: has go.mod      → NOT matched by the original rule
    ///
    /// Open the editor to EDIT the first (only) rule (cursor 0, press 'e').
    /// While the in-progress value reads "go.mod", the correct live count is 1
    /// (only repo_b matches go.mod).  The old code left "Cargo.toml" in the
    /// scratch clone AND added "go.mod", so the count would be 2 (both repos).
    #[test]
    fn edit_rule_live_count_does_not_double_count() {
        let dir_a = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("Cargo.toml"), "[package]").unwrap();
        let path_a = dir_a.path().display().to_string();

        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_b.path().join("go.mod"), "module x").unwrap();
        let path_b = dir_b.path().display().to_string();

        let inv = inv_with_repos(vec![
            repo(&path_a, &["Cargo.toml"], &[], &[]),
            repo(&path_b, &["go.mod"], &[], &[]),
        ]);

        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into()); // single rule at index 0

        let mut st = RulesState::open(d, &inv);
        assert_eq!(st.matched.len(), 1, "sanity: only repo_a matches initially");

        // Press 'e' to edit the selected rule (cursor=0, HasFile("Cargo.toml")).
        st.handle_key(KeyEvent::from(KeyCode::Char('e')), &inv);
        assert!(
            st.editor.is_some(),
            "editor must open on 'e' for an editable row"
        );
        assert_eq!(
            st.editor.as_ref().unwrap().editing,
            Some(0),
            "editing index must be 0"
        );
        // The pre-filled text is "Cargo.toml"; clear it by backspacing and type "go.mod".
        // Backspace 10 times (len("Cargo.toml") == 10).
        for _ in 0..10 {
            st.handle_key(KeyEvent::from(KeyCode::Backspace), &inv);
        }
        for c in "go.mod".chars() {
            st.handle_key(key(c), &inv);
        }
        // Correct live count: the scratch has Cargo.toml removed and go.mod added
        // → only repo_b matches → count = 1.
        assert_eq!(
            st.editor.as_ref().unwrap().live_count,
            Some(1),
            "live count while editing must reflect the edit REPLACING the old rule, not unioning them"
        );
    }

    #[test]
    fn builder_shows_live_match_count_while_typing() {
        // Two repos under /workspace/, one elsewhere. Typing the path-under prefix
        // should make the live count reflect 2 as soon as the prefix is entered.
        let inv = inv_with_repos(vec![
            repo("/workspace/a", &[], &[], &[]),
            repo("/workspace/b", &[], &[], &[]),
            repo("/other/c", &[], &[], &[]),
        ]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv); // open builder (kind-pick)
                                                                 // kind 0 = path under
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose path under
        assert_eq!(
            st.editor.as_ref().unwrap().live_count,
            None,
            "no count before a value is typed"
        );
        for c in "/workspace/".chars() {
            st.handle_key(key(c), &inv);
        }
        assert_eq!(
            st.editor.as_ref().unwrap().live_count,
            Some(2),
            "live count reflects the in-progress path-under rule (2 of 3 repos)"
        );
    }

    #[test]
    fn add_path_rule_via_builder() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv); // open builder (kind pick)
        assert!(st.editor.is_some());
        // kind list order: path under / has file / has any / contains. cursor 0 = path under.
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose "path under"
        assert!(st.editing_text(), "now in text entry");
        for c in "/workspace/".chars() {
            st.handle_key(key(c), &inv);
        }
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // commit
        assert_eq!(st.detect.path_prefixes, vec!["/workspace/".to_string()]);
        assert!(st.editor.is_none(), "builder closed after commit");
    }

    #[test]
    fn add_contains_rule_uses_two_inputs() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv);
        // move to "contains" (index 3): 3 Downs
        for _ in 0..3 {
            st.handle_key(KeyEvent::from(KeyCode::Down), &inv);
        }
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose contains -> file input
        for c in "requirements.txt".chars() {
            st.handle_key(key(c), &inv);
        }
        st.handle_key(KeyEvent::from(KeyCode::Tab), &inv); // focus word input
        for c in "torch".chars() {
            st.handle_key(key(c), &inv);
        }
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // commit
        assert_eq!(
            st.detect.content,
            vec![crate::profile::config::ContentRule {
                file: "requirements.txt".into(),
                word: "torch".into()
            }]
        );
    }

    #[test]
    fn esc_cancels_builder_without_adding() {
        let inv = inv_with_repos(vec![]);
        let mut st = RulesState::open(Detect::default(), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // into text entry (path under)
        st.handle_key(key('x'), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Esc), &inv); // cancel
        assert!(st.editor.is_none());
        assert!(
            st.detect.path_prefixes.is_empty(),
            "nothing added on cancel"
        );
    }

    #[test]
    fn cursor_follows_committed_rule() {
        let inv = inv_with_repos(vec![]);
        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into()); // row 0 (has file)
        let mut st = RulesState::open(d, &inv);
        st.cursor = 0;
        // Add a "has any" rule; it should land at index 1 and the cursor follow it.
        st.handle_key(KeyEvent::from(KeyCode::Char('a')), &inv);
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv); // has file
        st.handle_key(KeyEvent::from(KeyCode::Down), &inv); // has any
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // choose has any
        for c in "*.rs".chars() {
            st.handle_key(key(c), &inv);
        }
        st.handle_key(KeyEvent::from(KeyCode::Enter), &inv); // commit
        let rows = flatten(&st.detect);
        assert_eq!(
            rows[st.cursor],
            RuleRow::HasAny("*.rs".into()),
            "cursor should follow the just-committed rule, not stay at 0"
        );
    }

    #[test]
    fn remove_at_matches_flatten_indices_across_all_fields() {
        // indices: 0 path, 1 file, 2 glob, 3 content, 4 pkg-dep(legacy), 5 keyword(legacy)
        let mut d = detect();
        remove_at(&mut d, 3); // remove the content rule
        assert!(d.content.is_empty());
        assert_eq!(d.marker_files, vec!["Cargo.toml".to_string()]); // others intact

        let mut d2 = detect();
        remove_at(&mut d2, 5); // remove the deps_keyword (last legacy)
        assert!(d2.deps_keywords.is_empty());
        assert_eq!(d2.package_json_deps, vec!["svelte".to_string()]);

        let mut d3 = detect();
        remove_at(&mut d3, 0); // remove path prefix
        assert!(d3.path_prefixes.is_empty());
    }

    /// A repo that carries its own `.claude/profile` override must be excluded
    /// from BOTH the matched list and the near-miss list — detect rules do not
    /// classify override repos, so showing them either way misleads (Plan B
    /// review Minor #1). Verified end-to-end through `RulesState::recompute`.
    #[test]
    fn override_repos_excluded_from_matched_and_near() {
        // Repo A: plain, has Cargo.toml -> should be MATCHED by a has-file rule.
        let a = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("Cargo.toml"), "[package]").unwrap();
        let a_path = a.path().display().to_string();

        // Repo B: ALSO has Cargo.toml, but carries a .claude/profile override.
        // It must not appear as matched, and must not appear as a near-miss.
        let b = tempfile::tempdir().unwrap();
        std::fs::write(b.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::create_dir_all(b.path().join(".claude")).unwrap();
        std::fs::write(b.path().join(".claude").join("profile"), "frontend\n").unwrap();
        let b_path = b.path().display().to_string();

        let inv = inv_with_repos(vec![
            repo(&a_path, &["Cargo.toml"], &[], &[]),
            repo(&b_path, &["Cargo.toml"], &[], &[]),
        ]);
        let mut d = Detect::default();
        d.marker_files.push("Cargo.toml".into());
        let st = RulesState::open(d, &inv);

        // total_repos counts only override-free repos (A), not B.
        assert_eq!(
            st.total_repos, 1,
            "override repo B excluded from the scanned total"
        );
        // A is matched; B is absent from matched.
        assert_eq!(st.matched.len(), 1);
        assert_eq!(st.matched[0].path, a_path);
        assert!(
            !st.matched.iter().any(|m| m.path == b_path),
            "override repo must not be matched"
        );
        // B is absent from near as well (it was filtered before near-miss runs).
        assert!(
            !st.near.iter().any(|n| n.path == b_path),
            "override repo must not appear as a near-miss"
        );
    }
}
