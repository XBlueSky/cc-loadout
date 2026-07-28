use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use time::OffsetDateTime;

use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::Snapshot;
use crate::tui::theme;
use crate::tui::view::{Action, View};

/// Lists all scheduled tasks (prime + real) and emits run/delete actions.
pub struct TasksView {
    selected: usize,
    confirm_delete: bool,
}

impl TasksView {
    pub fn new() -> Self {
        TasksView {
            selected: 0,
            confirm_delete: false,
        }
    }

    /// Sorted slice of task rows (by id, ascending).
    fn rows(snap: &Snapshot) -> Vec<&crate::tui::snapshot::TaskRow> {
        let mut r: Vec<_> = snap.tasks.iter().collect();
        r.sort_by(|a, b| a.id.cmp(&b.id));
        r
    }

    fn clamp(&mut self, snap: &Snapshot) {
        let n = Self::rows(snap).len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    fn selected_id(&self, snap: &Snapshot) -> Option<String> {
        Self::rows(snap).get(self.selected).map(|r| r.id.clone())
    }
}

/// "HH:MM" from an `OffsetDateTime` (local, no seconds).
fn hhmm(t: OffsetDateTime) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

impl View for TasksView {
    fn title(&self) -> &str {
        "Tasks"
    }

    fn claims_key(&self, code: KeyCode) -> bool {
        if self.confirm_delete {
            // While confirming a delete, own y/n/Esc so they don't leak to the app.
            matches!(code, KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Esc)
        } else {
            matches!(
                code,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('r') | KeyCode::Char('d')
            )
        }
    }

    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm_delete {
            vec![("y", "confirm delete"), ("n/Esc", "cancel")]
        } else {
            vec![
                ("↑↓", "select"),
                ("r", "run now"),
                ("d", "delete"),
                ("·", "resume a run with: cc-loadout task resume <id>"),
            ]
        }
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &AppCtx, snap: &Snapshot) -> Option<Action> {
        self.clamp(snap);
        if self.confirm_delete {
            match key.code {
                KeyCode::Char('y') => {
                    self.confirm_delete = false;
                    return self.selected_id(snap).map(Action::RemoveTask);
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.confirm_delete = false;
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                self.selected += 1;
                self.clamp(snap);
                None
            }
            KeyCode::Char('r') => self.selected_id(snap).map(Action::RunTask),
            KeyCode::Char('d') => {
                if self.selected_id(snap).is_some() {
                    self.confirm_delete = true;
                }
                None
            }
            _ => None,
        }
    }

    fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        snap: &Snapshot,
        _now_ms: i64,
        _now_local: OffsetDateTime,
    ) {
        let rows = Self::rows(snap);

        if rows.is_empty() {
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "no tasks — add with `cc-loadout task add` or /cc-loadout:schedule",
                    theme::dim(),
                ))),
                area,
            );
            return;
        }

        // Layout: column header + list rows.
        use ratatui::layout::{Constraint, Layout};
        let list_h = (rows.len() as u16).min(area.height.saturating_sub(2));
        let sections = Layout::vertical([
            Constraint::Length(1),      // column header
            Constraint::Length(list_h), // task list
            Constraint::Min(0),         // confirm / empty footer
        ])
        .split(area);

        // ---- column header ----
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "  {:<20}{:<8}{:<14}{:<8}{:<18}{}",
                    "id", "kind", "account", "next", "times", "status"
                ),
                theme::dim(),
            ))),
            sections[0],
        );

        // ---- list ----
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| {
                let kind_str = match row.kind {
                    crate::task::config::Kind::Prime => "prime",
                    crate::task::config::Kind::Task => "task",
                };
                let next_str = row.next_fire.map(hhmm).unwrap_or_else(|| "—".into());
                let times_str = if row.times.is_empty() {
                    "—".to_string()
                } else {
                    row.times.join(" ")
                };
                let status_str = row.last_status.as_deref().unwrap_or("—");
                let spans = vec![Span::styled(
                    // The `kind` cell is `[xxxxx] ` = 8 chars, matching the
                    // header's `{:<8}` column so every column to its right
                    // (account/next/times/status) lines up with its label.
                    format!(
                        "{:<20}[{kind_str:<5}] {:<14}{:<8}{:<18}{status_str}",
                        row.id, row.account, next_str, times_str,
                    ),
                    theme::text(),
                )];
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(self.selected.min(rows.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▸ ")
            .highlight_spacing(HighlightSpacing::Always);
        f.render_stateful_widget(list, sections[1], &mut state);

        // ---- confirm-delete prompt ----
        if self.confirm_delete {
            if let Some(id) = rows.get(self.selected.min(rows.len() - 1)).map(|r| &r.id) {
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("Delete task '{id}'? "), theme::text()),
                        Span::styled("y", theme::accent()),
                        Span::styled(" / ", theme::faint()),
                        Span::styled("n", theme::accent()),
                    ])),
                    sections[2],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::snapshot::{AcctRow, TaskRow};
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// Build a minimal Snapshot with `tasks` set to the given rows.
    /// Follows `schedule.rs`'s `snap()` pattern — literal Snapshot construction.
    fn snap_with(rows: Vec<(&str, crate::task::config::Kind)>) -> Snapshot {
        Snapshot {
            accounts: vec![AcctRow {
                alias: "work".into(),
                email: "w@x".into(),
                org: Some("o".into()),
                is_active: true,
                expires_at_ms: None,
                has_refresh: false,
                window_start_epoch: None,
            }],
            cwd: std::path::PathBuf::from("/tmp"),
            profiles_json_exists: false,
            matched: vec![],
            applied_count: 0,
            priming: vec![],
            schedule: Default::default(),
            global_enabled: Vec::new(),
            tasks: rows
                .into_iter()
                .map(|(id, kind)| TaskRow {
                    id: id.into(),
                    kind,
                    account: "work".into(),
                    times: vec!["07:00".into()],
                    next_fire: None,
                    last_status: None,
                })
                .collect(),
            schedule_drift: false,
        }
    }

    /// Build a minimal AppCtx. Follows `schedule.rs`'s `ctx()` pattern.
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
    fn r_on_selected_row_emits_run_task() {
        let s = snap_with(vec![("weekly", crate::task::config::Kind::Task)]);
        let mut v = TasksView::new();
        let c = ctx();
        let act = v.on_key(key(KeyCode::Char('r')), &c, &s);
        assert!(
            matches!(act, Some(Action::RunTask(ref id)) if id == "weekly"),
            "expected RunTask(\"weekly\"), got {act:?}"
        );
    }

    #[test]
    fn d_then_confirm_emits_remove_task() {
        let s = snap_with(vec![("weekly", crate::task::config::Kind::Task)]);
        let mut v = TasksView::new();
        let c = ctx();
        assert!(
            v.on_key(key(KeyCode::Char('d')), &c, &s).is_none(),
            "first d arms confirm, emits nothing"
        );
        assert!(
            v.claims_key(KeyCode::Char('y')),
            "confirm state must claim 'y'"
        );
        let act = v.on_key(key(KeyCode::Char('y')), &c, &s);
        assert!(
            matches!(act, Some(Action::RemoveTask(ref id)) if id == "weekly"),
            "expected RemoveTask(\"weekly\"), got {act:?}"
        );
    }

    #[test]
    fn down_moves_selection_and_clamps() {
        let s = snap_with(vec![
            ("a", crate::task::config::Kind::Prime),
            ("b", crate::task::config::Kind::Task),
        ]);
        let mut v = TasksView::new();
        let c = ctx();
        v.on_key(key(KeyCode::Down), &c, &s);
        v.on_key(key(KeyCode::Down), &c, &s); // clamps at last
                                              // rows sorted by id: a, b -> selected=1 -> "b"
        let act = v.on_key(key(KeyCode::Char('r')), &c, &s);
        assert!(
            matches!(act, Some(Action::RunTask(ref id)) if id == "b"),
            "expected RunTask(\"b\"), got {act:?}"
        );
    }
}
