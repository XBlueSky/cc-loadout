use std::collections::{BTreeMap, BTreeSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use crate::account::format_duration_short;
use crate::account::timing::next_fire;
use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::Snapshot;
use crate::tui::theme;
use crate::tui::view::{Action, View};

/// Daily-prime scheduler over all accounts. A master list of accounts; the
/// focused account's prime times are edited as a 24-hour toggle grid (hour
/// granularity — enough to keep each account's 5h window warm). `w` writes.
pub struct ScheduleView {
    cursor: usize,    // focused account (master)
    grid: Option<u8>, // Some(hour 0..24) = grid-edit mode + cursor hour
    working: Option<BTreeMap<String, Vec<String>>>,
}

impl ScheduleView {
    pub fn new() -> Self {
        ScheduleView {
            cursor: 0,
            grid: None,
            working: None,
        }
    }

    /// Lazily seed the working copy from the snapshot's current schedule.
    fn working_mut(&mut self, snap: &Snapshot) -> &mut BTreeMap<String, Vec<String>> {
        if self.working.is_none() {
            let mut m = BTreeMap::new();
            for a in &snap.accounts {
                if let Some(times) = snap.schedule_times(&a.alias) {
                    if !times.is_empty() {
                        m.insert(a.alias.clone(), times.clone());
                    }
                }
            }
            self.working = Some(m);
        }
        self.working.as_mut().unwrap()
    }

    fn aliases(snap: &Snapshot) -> Vec<String> {
        snap.accounts.iter().map(|a| a.alias.clone()).collect()
    }

    fn clamp(&mut self, snap: &Snapshot) {
        let n = Self::aliases(snap).len();
        if n == 0 {
            self.cursor = 0;
        } else if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    /// Times for `alias`. Once editing has begun the working copy is the
    /// authoritative full state — a missing alias means *unscheduled*, NOT
    /// "fall back to the snapshot" (otherwise toggling every hour off would let
    /// the snapshot's old hours reappear). Only an untouched view reads the snapshot.
    fn times_for(&self, alias: &str, snap: &Snapshot) -> Vec<String> {
        match &self.working {
            Some(w) => w.get(alias).cloned().unwrap_or_default(),
            None => snap.schedule_times(alias).cloned().unwrap_or_default(),
        }
    }

    /// Toggle hour `h` (as HH:00) for `alias` in the working copy.
    fn toggle_hour(&mut self, alias: &str, h: u8, snap: &Snapshot) {
        let mut hours = hours_of(&self.times_for(alias, snap));
        if !hours.insert(h) {
            hours.remove(&h);
        }
        let times = times_of(&hours);
        let w = self.working_mut(snap);
        if times.is_empty() {
            w.remove(alias);
        } else {
            w.insert(alias.to_string(), times);
        }
    }

    /// Build the alias->times map from the working copy (or the snapshot if untouched).
    pub fn working_schedule(&self, snap: &Snapshot) -> BTreeMap<String, Vec<String>> {
        let src: BTreeMap<String, Vec<String>> = match &self.working {
            Some(w) => w.clone(),
            None => {
                let mut m = BTreeMap::new();
                for a in &snap.accounts {
                    if let Some(t) = snap.schedule_times(&a.alias) {
                        if !t.is_empty() {
                            m.insert(a.alias.clone(), t.clone());
                        }
                    }
                }
                m
            }
        };
        src.into_iter()
            .filter(|(_, times)| !times.is_empty())
            .collect()
    }
}

/// Hours (0-23) that have at least one scheduled time.
fn hours_of(times: &[String]) -> BTreeSet<u8> {
    times
        .iter()
        .filter_map(|t| t.split_once(':')?.0.parse::<u8>().ok())
        .collect()
}

/// Sorted "HH:00" strings for a set of hours.
fn times_of(hours: &BTreeSet<u8>) -> Vec<String> {
    hours.iter().map(|h| format!("{h:02}:00")).collect()
}

/// The 24-hour toggle grid for `alias`: scheduled hours in accent, the grid
/// cursor (when editing) on a selection background; a contextual hint.
fn grid_lines(alias: &str, times: &[String], cursor: Option<u8>) -> Vec<Line<'static>> {
    let hours = hours_of(times);
    let hint = if cursor.is_some() {
        "←→ hour · ↑↓ row · space toggle · esc back"
    } else {
        "⏎ edit hours"
    };
    let mut out = vec![
        Line::from(vec![
            Span::styled(format!("{alias} — prime hours   "), theme::text()),
            Span::styled(format!("({hint})"), theme::faint()),
        ]),
        Line::from(""),
    ];
    for row in 0u8..2 {
        let mut spans = vec![Span::raw("  ")];
        for col in 0u8..12 {
            let h = row * 12 + col;
            let mut st = if hours.contains(&h) {
                theme::accent()
            } else {
                theme::faint()
            };
            if cursor == Some(h) {
                st = st.patch(theme::selection());
            }
            spans.push(Span::styled(format!("{h:02} "), st));
        }
        out.push(Line::from(spans));
    }
    out
}

impl View for ScheduleView {
    fn title(&self) -> &str {
        "Schedule"
    }

    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.grid.is_some() {
            vec![
                ("←→", "hour"),
                ("↑↓", "row"),
                ("space", "toggle"),
                ("esc", "back"),
                ("w", "write"),
            ]
        } else {
            vec![
                ("↑↓", "move"),
                ("⏎", "edit hours"),
                ("space", "clear"),
                ("w", "write"),
            ]
        }
    }

    fn claims_key(&self, code: ratatui::crossterm::event::KeyCode) -> bool {
        use ratatui::crossterm::event::KeyCode;
        // Grid-edit is a modal hour editor: claim everything but Tab/BackTab
        // (the escape hatch). This exactly preserves the old
        // `wants_raw_input == self.grid.is_some()` behavior (Esc/Enter exit the
        // grid, ←/→/h/l move, space toggles, w writes — all reach on_key; q is
        // a no-op inside the grid rather than quitting).
        self.grid.is_some() && !matches!(code, KeyCode::Tab | KeyCode::BackTab)
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &AppCtx, snap: &Snapshot) -> Option<Action> {
        self.clamp(snap);
        let aliases = Self::aliases(snap);

        // Grid-edit mode: arrows move the hour cursor, space toggles, esc exits.
        if let Some(h) = self.grid {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') => self.grid = Some((h + 23) % 24),
                KeyCode::Right | KeyCode::Char('l') => self.grid = Some((h + 1) % 24),
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    self.grid = Some((h + 12) % 24)
                }
                KeyCode::Char(' ') => {
                    if let Some(alias) = aliases.get(self.cursor).cloned() {
                        self.toggle_hour(&alias, h, snap);
                    }
                }
                KeyCode::Esc | KeyCode::Enter => self.grid = None,
                KeyCode::Char('w') => {
                    return Some(Action::WriteSchedule(self.working_schedule(snap)))
                }
                _ => {}
            }
            return None;
        }

        // List mode.
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !aliases.is_empty() {
                    self.cursor = (self.cursor + 1) % aliases.len();
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !aliases.is_empty() {
                    let n = aliases.len();
                    self.cursor = (self.cursor + n - 1) % n;
                }
                None
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                if let Some(alias) = aliases.get(self.cursor).cloned() {
                    // Start the grid cursor at the first scheduled hour, else 09:00.
                    let start = hours_of(&self.times_for(&alias, snap))
                        .iter()
                        .next()
                        .copied()
                        .unwrap_or(9);
                    self.grid = Some(start);
                }
                None
            }
            KeyCode::Char(' ') => {
                if let Some(alias) = aliases.get(self.cursor).cloned() {
                    self.working_mut(snap).remove(&alias);
                }
                None
            }
            KeyCode::Char('w') => Some(Action::WriteSchedule(self.working_schedule(snap))),
            _ => None,
        }
    }

    fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        snap: &Snapshot,
        now_ms: i64,
        now_local: OffsetDateTime,
    ) {
        let aliases = Self::aliases(snap);
        let now_epoch = now_ms / 1000;

        // Layout: help, blank, header, list, rule, grid detail.
        let n = aliases.len().max(1) as u16;
        let max_list = area.height.saturating_sub(3 + 1 + 5).max(1);
        let list_h = n.min(max_list);
        let rows = Layout::vertical([
            Constraint::Length(1),      // help
            Constraint::Length(1),      // blank
            Constraint::Length(1),      // column header
            Constraint::Length(list_h), // account list
            Constraint::Length(1),      // rule
            Constraint::Min(0),         // grid detail
        ])
        .split(area);

        // ---- help / drift warning ----
        // When the live crontab no longer matches the saved schedule (a write that
        // never landed, or a crontab wiped externally), say so persistently and point
        // at the fix — `w` re-installs the current schedule into cron.
        let help = if snap.schedule_drift {
            Line::from(vec![
                Span::styled(
                    "⚠ crontab is out of sync with the saved schedule — ",
                    theme::alert(),
                ),
                Span::styled("press ", theme::dim()),
                Span::styled("w", theme::accent()),
                Span::styled(" to re-install into cron.", theme::dim()),
            ])
        } else {
            Line::from(Span::styled(
                "Scheduled times auto-prime each account to keep its 5h window warm.",
                theme::dim(),
            ))
        };
        f.render_widget(Paragraph::new(help), rows[0]);

        // ---- column header (4 leading: 2 highlight + 2 marker) ----
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("    {:<12}{:<22}{}", "alias", "times", "next"),
                theme::dim(),
            ))),
            rows[2],
        );

        // ---- master list ----
        let items: Vec<ListItem> = aliases
            .iter()
            .map(|alias| {
                let times = self.times_for(alias, snap);
                let scheduled = !times.is_empty();
                let marker_style = if scheduled {
                    theme::accent()
                } else {
                    theme::dim()
                };
                let marker = if scheduled { "✓" } else { " " };
                let times_str = if times.is_empty() {
                    "—".to_string()
                } else {
                    times.join(" ")
                };
                let mut spans = vec![
                    Span::styled(format!("{marker} "), marker_style),
                    Span::styled(format!("{alias:<12}{times_str:<22}"), theme::text()),
                ];
                if let Some(nf) = next_fire(&times, now_local) {
                    let rel = format_duration_short((nf.unix_timestamp() - now_epoch) * 1000)
                        .unwrap_or_else(|| "soon".into());
                    spans.push(Span::styled(format!("in {rel}"), theme::dim()));
                } else {
                    spans.push(Span::styled("—", theme::faint()));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        if !aliases.is_empty() {
            state.select(Some(self.cursor.min(aliases.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▸ ")
            .highlight_spacing(HighlightSpacing::Always);
        f.render_stateful_widget(list, rows[3], &mut state);

        // ---- rule ----
        let rule = "─".repeat(rows[4].width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, theme::faint()))),
            rows[4],
        );

        // ---- grid detail for the focused account ----
        if let Some(alias) = aliases.get(self.cursor.min(aliases.len().saturating_sub(1))) {
            let times = self.times_for(alias, snap);
            let mut detail = grid_lines(alias, &times, self.grid);
            // Show when the scheduled prime last ran (did it actually fire?).
            if let Some(lp) = snap.last_primed(alias) {
                let ago = format_duration_short((now_epoch - lp) * 1000)
                    .unwrap_or_else(|| "just now".into());
                detail.push(Line::from(""));
                detail.push(Line::from(vec![
                    Span::styled("  last primed  ", theme::dim()),
                    Span::styled(format!("{ago} ago"), theme::text()),
                ]));
            }
            f.render_widget(Paragraph::new(detail), rows[5]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::snapshot::{AcctRow, Snapshot};
    use ratatui::crossterm::event::KeyEvent;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    fn snap() -> Snapshot {
        Snapshot {
            accounts: vec![
                AcctRow {
                    alias: "work".into(),
                    email: "w@x".into(),
                    org: Some("o".into()),
                    is_active: true,
                    expires_at_ms: None,
                    has_refresh: false,
                    window_start_epoch: None,
                },
                AcctRow {
                    alias: "personal".into(),
                    email: "p@x".into(),
                    org: Some("o".into()),
                    is_active: false,
                    expires_at_ms: None,
                    has_refresh: false,
                    window_start_epoch: None,
                },
            ],
            cwd: std::path::PathBuf::from("/tmp"),
            profiles_json_exists: false,
            matched: vec![],
            applied_count: 0,
            priming: vec![],
            schedule: std::collections::BTreeMap::new(),
            global_enabled: Vec::new(),
            tasks: Vec::new(),
            schedule_drift: false,
            scope_drift: Vec::new(),
        }
    }

    fn ctx() -> AppCtx {
        let h = std::env::temp_dir();
        AppCtx {
            store: crate::account::store::Store::new(&h),
            claude: crate::account::paths::resolve(&h, None),
            home: h.clone(),
            data_root: h.clone(),
            cfg_path: h.join("none.json"),
            registry_path: h.join("nope-registry.json"),
            cwd: h,
        }
    }

    #[test]
    fn enter_grid_then_toggle_sets_time() {
        let mut v = ScheduleView::new();
        let (s, c) = (snap(), ctx());
        v.on_key(k(KeyCode::Enter), &c, &s); // enter grid for "work" (cursor at 09)
        assert!(v.claims_key(KeyCode::Char('x')), "grid mode captures input");
        v.on_key(k(KeyCode::Char(' ')), &c, &s); // toggle hour 09 -> 09:00
        let sched = v.working_schedule(&s);
        assert_eq!(sched.get("work").unwrap(), &vec!["09:00".to_string()]);
    }

    #[test]
    fn arrows_move_cursor_then_toggle() {
        let mut v = ScheduleView::new();
        let (s, c) = (snap(), ctx());
        v.on_key(k(KeyCode::Enter), &c, &s); // grid, cursor 09
        v.on_key(k(KeyCode::Left), &c, &s); // -> 08
        v.on_key(k(KeyCode::Char(' ')), &c, &s); // toggle 08 -> 08:00
        assert_eq!(
            v.working_schedule(&s).get("work").unwrap(),
            &vec!["08:00".to_string()]
        );
    }

    #[test]
    fn space_clears_in_list_mode() {
        let mut v = ScheduleView::new();
        let (s, c) = (snap(), ctx());
        v.on_key(k(KeyCode::Enter), &c, &s);
        v.on_key(k(KeyCode::Char(' ')), &c, &s); // schedule work
        v.on_key(k(KeyCode::Esc), &c, &s); // back to list
        assert!(v.working_schedule(&s).contains_key("work"));
        v.on_key(k(KeyCode::Char(' ')), &c, &s); // list-mode space clears focused
        assert!(!v.working_schedule(&s).contains_key("work"));
    }

    #[test]
    fn esc_exits_grid_keeping_edits() {
        let mut v = ScheduleView::new();
        let (s, c) = (snap(), ctx());
        v.on_key(k(KeyCode::Enter), &c, &s);
        v.on_key(k(KeyCode::Char(' ')), &c, &s);
        v.on_key(k(KeyCode::Esc), &c, &s);
        assert!(
            !v.claims_key(KeyCode::Char('x')),
            "esc returns to list mode"
        );
        assert!(
            v.working_schedule(&s).contains_key("work"),
            "toggled edit persists after esc"
        );
    }

    #[test]
    fn toggling_all_hours_off_unschedules_and_stays_off() {
        let mut v = ScheduleView::new();
        let mut s = snap();
        // work starts scheduled at 06:00 in the snapshot.
        s.schedule.insert("work".into(), vec!["06:00".into()]);
        let c = ctx();
        v.on_key(k(KeyCode::Enter), &c, &s); // grid for work, cursor at 06
        v.on_key(k(KeyCode::Char(' ')), &c, &s); // toggle 06 off -> empty
        assert!(
            !v.working_schedule(&s).contains_key("work"),
            "work is unscheduled after clearing its only hour"
        );
        assert!(
            v.times_for("work", &s).is_empty(),
            "must not resurrect the snapshot's hours"
        );
    }

    #[test]
    fn w_emits_write_schedule() {
        let mut v = ScheduleView::new();
        let (s, c) = (snap(), ctx());
        match v.on_key(k(KeyCode::Char('w')), &c, &s) {
            Some(Action::WriteSchedule(_)) => {}
            other => panic!("expected WriteSchedule, got {other:?}"),
        }
    }

    fn rendered(snap: &Snapshot) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let v = ScheduleView::new();
        let mut t = Terminal::new(TestBackend::new(80, 16)).unwrap();
        let now = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        t.draw(|f| v.render(f, f.area(), snap, 1_000_000_000, now))
            .unwrap();
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn drift_banner_shown_when_crontab_out_of_sync() {
        let mut s = snap();
        s.schedule_drift = true;
        let text = rendered(&s);
        assert!(
            text.contains("out of sync"),
            "a drifted crontab must show a warning banner; got: {text}"
        );
    }

    #[test]
    fn no_drift_banner_when_in_sync() {
        let s = snap(); // schedule_drift defaults to false
        let text = rendered(&s);
        assert!(!text.contains("out of sync"), "no warning when in sync");
        assert!(
            text.contains("auto-prime"),
            "normal help shown when in sync; got: {text}"
        );
    }
}
