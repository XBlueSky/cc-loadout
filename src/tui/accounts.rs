use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, HighlightSpacing, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::account::format_duration_short;
use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::{AcctRow, Snapshot};
use crate::tui::theme;
use crate::tui::view::{Action, View};
use crate::tui::widgets::{fine_bar, token_label, window_remaining_ms, WINDOW_MS};

/// Overlay state for the Accounts tab.
enum Modal {
    None,
    ConfirmRemove(String),
}

/// The Accounts tab: a navigable account list with per-row token state.
pub struct AccountsView {
    cursor: usize,
    modal: Modal,
}

impl AccountsView {
    pub fn new() -> Self {
        AccountsView {
            cursor: 0,
            modal: Modal::None,
        }
    }

    /// Clamp the cursor to the current row count (rows can shrink after a remove).
    fn clamp(&mut self, snap: &Snapshot) {
        let n = snap.accounts.len();
        if n == 0 {
            self.cursor = 0;
        } else if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    /// The alias under the cursor, if any.
    fn selected_alias<'a>(&self, snap: &'a Snapshot) -> Option<&'a str> {
        snap.accounts.get(self.cursor).map(|a| a.alias.as_str())
    }
}

/// Left-pad `s` to `w` columns, or truncate with a trailing ellipsis when it is
/// longer, so master-list columns stay aligned no matter how long an alias/org
/// is (a Google account's auto "…'s Organization" would otherwise overflow and
/// shove the window gauge + token out of their columns).
fn fit(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        format!("{s:<w$}")
    } else if w == 0 {
        String::new()
    } else {
        let head: String = s.chars().take(w - 1).collect();
        format!("{head}\u{2026}")
    }
}

/// The detail pane for the focused account: full email + org, token, the 5h
/// window gauge, and its prime schedule — using the same aligned sub-label
/// grammar as the Overview tab.
fn account_detail(a: &AcctRow, snap: &Snapshot, now_ms: i64) -> Vec<Line<'static>> {
    let mut out: Vec<Line> = Vec::new();

    // Identity: alias · email. (org gets its own sub-row below, so a long
    // auto-generated org name doesn't clip the email mid-string.)
    out.push(Line::from(vec![
        Span::styled(a.alias.clone(), theme::accent()),
        Span::styled("  ·  ", theme::faint()),
        Span::styled(a.email.clone(), theme::text()),
    ]));
    out.push(Line::from(""));

    // org (aligned sub-row, value at column 12 — matches token/window).
    if let Some(org) = &a.org {
        out.push(Line::from(vec![
            Span::styled("  org       ", theme::dim()),
            Span::styled(org.clone(), theme::dim()),
        ]));
        out.push(Line::from(""));
    }

    // token (aligned sub-row, values at column 12).
    let (token, urgent) = token_label(a.expires_at_ms, a.has_refresh, now_ms);
    let token_style = if urgent { theme::alert() } else { theme::dim() };
    out.push(Line::from(vec![
        Span::styled("  token     ", theme::dim()),
        Span::styled(token, token_style),
    ]));
    out.push(Line::from(""));

    // window gauge + remaining.
    let rem = window_remaining_ms(a.window_start_epoch, now_ms);
    let frac = rem.map(|r| r as f64 / WINDOW_MS as f64).unwrap_or(0.0);
    let (filled, empty) = fine_bar(frac, 12);
    let wlabel = match rem.and_then(format_duration_short) {
        Some(s) => format!("  {s} left"),
        None => "  idle".to_string(),
    };
    let mut wl = vec![
        Span::styled("  window    ", theme::dim()),
        Span::styled("▏", theme::faint()),
    ];
    if !filled.is_empty() {
        wl.push(Span::styled(filled, theme::accent()));
    }
    if !empty.is_empty() {
        wl.push(Span::styled(empty, theme::faint()));
    }
    wl.push(Span::styled("▕", theme::faint()));
    wl.push(Span::styled(wlabel, theme::text()));
    out.push(Line::from(wl));
    out.push(Line::from(""));

    // schedule: times + next fire, or "not scheduled".
    let sched = match snap.schedule_times(&a.alias) {
        Some(times) if !times.is_empty() => {
            let mut s = times.join(", ");
            if let Some(nf) = snap
                .priming
                .iter()
                .find(|p| p.alias == a.alias)
                .and_then(|p| p.next_fire)
            {
                let rel = format_duration_short((nf.unix_timestamp() - now_ms / 1000) * 1000)
                    .unwrap_or_else(|| "soon".into());
                s.push_str(&format!("   ·   next in {rel}"));
            }
            Span::styled(s, theme::text())
        }
        _ => Span::styled("not scheduled".to_string(), theme::dim()),
    };
    out.push(Line::from(vec![
        Span::styled("  schedule  ", theme::dim()),
        sched,
    ]));

    out
}

impl View for AccountsView {
    fn title(&self) -> &str {
        "Accounts"
    }

    fn claims_key(&self, code: ratatui::crossterm::event::KeyCode) -> bool {
        use ratatui::crossterm::event::KeyCode;
        // The remove-confirmation modal claims Esc (cancel) so it doesn't quit.
        matches!(self.modal, Modal::ConfirmRemove(_)) && matches!(code, KeyCode::Esc)
    }

    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("↑↓", "move"),
            ("⏎", "switch"),
            ("p", "prime"),
            ("d", "remove"),
            ("R", "relaunch"),
        ]
    }

    fn on_key(&mut self, key: KeyEvent, _ctx: &AppCtx, snap: &Snapshot) -> Option<Action> {
        self.clamp(snap);
        // Modal takes keys first.
        if let Modal::ConfirmRemove(alias) = &self.modal {
            return match key.code {
                KeyCode::Char('y') => {
                    let a = alias.clone();
                    self.modal = Modal::None;
                    Some(Action::RemoveAccount(a))
                }
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.modal = Modal::None;
                    None
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                if !snap.accounts.is_empty() {
                    self.cursor = (self.cursor + 1) % snap.accounts.len();
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if !snap.accounts.is_empty() {
                    let n = snap.accounts.len();
                    self.cursor = (self.cursor + n - 1) % n;
                }
                None
            }
            KeyCode::Enter => self
                .selected_alias(snap)
                .map(|a| Action::Switch(a.to_string())),
            KeyCode::Char('p') => self
                .selected_alias(snap)
                .map(|a| Action::Prime(a.to_string())),
            KeyCode::Char('d') => {
                if let Some(a) = self.selected_alias(snap) {
                    self.modal = Modal::ConfirmRemove(a.to_string());
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
        now_ms: i64,
        _now_local: time::OffsetDateTime,
    ) {
        // Master-detail layout: column header, the list (sized to the accounts,
        // capped so the detail pane always fits), a faint rule, then the detail
        // pane for the focused account.
        let n = snap.accounts.len().max(1) as u16;
        // Reserve: header(1) + gap(1) + rule(1) + airy detail(~7).
        let max_list = area.height.saturating_sub(10).max(1);
        let list_h = n.min(max_list);
        let rows = Layout::vertical([
            Constraint::Length(1),      // column header
            Constraint::Length(1),      // breathing gap
            Constraint::Length(list_h), // master list
            Constraint::Length(1),      // rule
            Constraint::Min(0),         // detail pane
        ])
        .split(area);

        // ---- column header (aligned: 2 highlight + 2 marker = 4 leading) ----
        let header = Line::from(Span::styled(
            format!(
                "    {:<12}{:<12}{:<10}{}",
                "alias", "org", "window", "token"
            ),
            theme::dim(),
        ));
        f.render_widget(Paragraph::new(header), rows[0]);

        // ---- master list (marker · alias · org · window gauge · token) ----
        let items: Vec<ListItem> = snap
            .accounts
            .iter()
            .map(|a| {
                let (token, urgent) = token_label(a.expires_at_ms, a.has_refresh, now_ms);
                let token_style = if urgent { theme::alert() } else { theme::dim() };
                let (marker, marker_style) = if a.is_active {
                    ("●", theme::accent())
                } else {
                    (" ", theme::dim())
                };
                let org = a.org.as_deref().unwrap_or("—");
                let rem = window_remaining_ms(a.window_start_epoch, now_ms);
                let frac = rem.map(|r| r as f64 / WINDOW_MS as f64).unwrap_or(0.0);
                let (filled, empty) = fine_bar(frac, 6);

                let mut spans = vec![
                    Span::styled(format!("{marker} "), marker_style),
                    Span::styled(
                        format!("{}{}", fit(&a.alias, 12), fit(org, 12)),
                        theme::text(),
                    ),
                    Span::styled("▏", theme::faint()),
                ];
                if !filled.is_empty() {
                    spans.push(Span::styled(filled, theme::accent()));
                }
                if !empty.is_empty() {
                    spans.push(Span::styled(empty, theme::faint()));
                }
                spans.push(Span::styled("▕  ", theme::faint()));
                spans.push(Span::styled(token, token_style));

                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        if !snap.accounts.is_empty() {
            state.select(Some(self.cursor.min(snap.accounts.len() - 1)));
        }
        let list = List::new(items)
            .highlight_style(theme::selection())
            .highlight_symbol("▸ ")
            .highlight_spacing(HighlightSpacing::Always);
        f.render_stateful_widget(list, rows[2], &mut state);

        // ---- rule ----
        let rule = "─".repeat(rows[3].width as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, theme::faint()))),
            rows[3],
        );

        // ---- detail pane for the focused account ----
        if let Some(a) = snap
            .accounts
            .get(self.cursor.min(snap.accounts.len().saturating_sub(1)))
        {
            f.render_widget(Paragraph::new(account_detail(a, snap, now_ms)), rows[4]);
        }

        if let Modal::ConfirmRemove(alias) = &self.modal {
            use crate::tui::widgets::centered_rect;
            use ratatui::widgets::Clear;
            let r = centered_rect(60, 20, area);
            let body = Paragraph::new(format!(
                "Remove '{alias}'? This deletes its credential snapshot.\n\n  y = yes    n = no"
            ))
            .block(
                Block::bordered()
                    .style(theme::panel())
                    .border_type(theme::BORDER)
                    .border_style(theme::accent_dim())
                    .title("Confirm remove"),
            );
            f.render_widget(Clear, r);
            f.render_widget(body, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::snapshot::{AcctRow, Snapshot};
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyEvent;
    use ratatui::Terminal;

    #[test]
    fn fit_pads_short_and_elides_long_to_exact_width() {
        // Every result is exactly `w` columns, so table columns stay aligned.
        assert_eq!(fit("abc", 6), "abc   ");
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdefgh", 6), "abcde\u{2026}");
        for s in ["", "x", "short", "way-too-long-organization-name"] {
            assert_eq!(
                fit(s, 6).chars().count(),
                6,
                "fit must be exactly 6 cols: {s}"
            );
        }
    }

    fn snap(n: usize) -> Snapshot {
        let accounts = (0..n)
            .map(|i| AcctRow {
                alias: format!("acct{i}"),
                email: format!("a{i}@x"),
                org: Some("acme".into()),
                is_active: i == 0,
                expires_at_ms: Some(2_000_000_000_000),
                has_refresh: true,
                window_start_epoch: None,
            })
            .collect();
        Snapshot {
            accounts,
            cwd: std::path::PathBuf::from("/tmp/x"),
            profiles_json_exists: false,
            matched: vec![],
            applied_count: 0,
            priming: vec![],
            schedule: Default::default(),
            global_enabled: Vec::new(),
            tasks: Vec::new(),
            schedule_drift: false,
        }
    }

    fn now_local_at(secs: i64) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap()
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
    fn down_up_wrap_the_cursor() {
        let mut v = AccountsView::new();
        let s = snap(3);
        let c = ctx();
        assert_eq!(v.selected_alias(&s), Some("acct0"));
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        assert_eq!(v.selected_alias(&s), Some("acct1"));
        v.on_key(KeyEvent::from(KeyCode::Up), &c, &s);
        v.on_key(KeyEvent::from(KeyCode::Up), &c, &s);
        assert_eq!(v.selected_alias(&s), Some("acct2"), "Up wraps to last");
    }

    #[test]
    fn enter_emits_switch_for_selected() {
        let mut v = AccountsView::new();
        let s = snap(2);
        let c = ctx();
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s);
        assert!(
            matches!(
                v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s),
                Some(Action::Switch(a)) if a == "acct1"
            ),
            "Enter emits Switch(acct1)"
        );
        assert!(
            matches!(
                v.on_key(KeyEvent::from(KeyCode::Char('p')), &c, &s),
                Some(Action::Prime(a)) if a == "acct1"
            ),
            "p emits Prime(acct1)"
        );
    }

    #[test]
    fn d_then_y_confirms_remove() {
        let mut v = AccountsView::new();
        let s = snap(2);
        let c = ctx();
        v.on_key(KeyEvent::from(KeyCode::Down), &c, &s); // select acct1
        assert!(
            v.on_key(KeyEvent::from(KeyCode::Char('d')), &c, &s)
                .is_none(),
            "d opens modal"
        );
        assert!(
            matches!(
                v.on_key(KeyEvent::from(KeyCode::Char('y')), &c, &s),
                Some(Action::RemoveAccount(a)) if a == "acct1"
            ),
            "y confirms remove of acct1"
        );
    }

    #[test]
    fn d_then_n_cancels() {
        let mut v = AccountsView::new();
        let s = snap(2);
        let c = ctx();
        v.on_key(KeyEvent::from(KeyCode::Char('d')), &c, &s);
        assert!(
            v.on_key(KeyEvent::from(KeyCode::Char('n')), &c, &s)
                .is_none(),
            "n cancels"
        );
        // after cancel, nav works again
        assert!(
            matches!(
                v.on_key(KeyEvent::from(KeyCode::Enter), &c, &s),
                Some(Action::Switch(a)) if a == "acct0"
            ),
            "Enter after cancel emits Switch(acct0)"
        );
    }

    #[test]
    fn renders_rows_with_active_marker() {
        let backend = TestBackend::new(80, 14);
        let mut t = Terminal::new(backend).unwrap();
        let v = AccountsView::new();
        let s = snap(2);
        t.draw(|f| {
            v.render(
                f,
                f.area(),
                &s,
                1_000_000_000_000,
                now_local_at(1_000_000_000_000 / 1000),
            )
        })
        .unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("acct0"), "first account alias shown");
        assert!(text.contains("acct1"), "second account alias shown");
        assert!(text.contains("●"), "active marker shown");
        // Email now lives in the detail pane for the focused account (acct0).
        assert!(
            text.contains("a0@x"),
            "focused account email shown in detail"
        );
    }
}
