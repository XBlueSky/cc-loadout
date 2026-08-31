use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::account::format_duration_short;
use crate::tui::ctx::AppCtx;
use crate::tui::snapshot::Snapshot;
use crate::tui::theme;
use crate::tui::view::{Action, View};
use crate::tui::widgets::{fine_bar, token_label, window_remaining_ms, WINDOW_MS};

/// The live landing tab: active account + 5h window, cwd profile, next prime.
pub struct Overview;

impl Overview {
    pub fn new() -> Self {
        Overview
    }

    fn lines(&self, snap: &Snapshot, now_ms: i64) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();

        // ---- ACCOUNT ----
        lines.push(section_header("ACCOUNT"));
        match snap.accounts.iter().find(|a| a.is_active) {
            Some(a) => {
                // Identity: bold accent alias, then org (dim-dot separator) only
                // when the account actually has one.
                let mut identity = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("● {}", a.alias),
                        theme::accent().add_modifier(Modifier::BOLD),
                    ),
                ];
                if let Some(org) = &a.org {
                    identity.push(Span::styled("  ·  ", theme::faint()));
                    identity.push(Span::styled(org.clone(), theme::text()));
                }
                lines.push(Line::from(identity));
                lines.push(Line::from(""));
                // Token-lifetime bar (remaining over the 5h visual scale shared
                // with the window). Value goes alert only when < 1h / expired.
                let (token, urgent) = token_label(a.expires_at_ms, a.has_refresh, now_ms);
                let token_style = if urgent { theme::alert() } else { theme::dim() };
                let token_frac = a
                    .expires_at_ms
                    .map(|e| (e - now_ms) as f64 / WINDOW_MS as f64)
                    .unwrap_or(0.0);
                lines.push(bar_line("  token   ", token_frac, &token, token_style));
                // Blank row between the two bars: adjacent rows of full-height
                // block fill would otherwise merge into one solid rectangle.
                lines.push(Line::from(""));
                let rem = window_remaining_ms(a.window_start_epoch, now_ms);
                let window_frac = rem.map(|r| r as f64 / WINDOW_MS as f64).unwrap_or(0.0);
                let window_label = match rem.and_then(format_duration_short) {
                    Some(s) => format!("{s} / 5h"),
                    None => "idle".to_string(),
                };
                lines.push(bar_line(
                    "  window  ",
                    window_frac,
                    &window_label,
                    theme::text(),
                ));
            }
            None => lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "(none — run: cc-loadout account add <alias>)",
                    theme::text(),
                ),
            ])),
        }
        lines.push(Line::from(""));

        // ---- PROFILE ----
        lines.push(section_header("PROFILE"));
        let cwd_name = snap
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| snap.cwd.display().to_string());
        if !snap.profiles_json_exists {
            lines.push(value_line(&cwd_name, "no profiles.json", theme::dim()));
        } else {
            // Repo name, then matched profiles as accent chips + applied count.
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(cwd_name.clone(), theme::text()),
            ]));
            lines.push(Line::from(""));
            let mut row = vec![Span::raw("  ")];
            if snap.matched.is_empty() {
                row.push(Span::styled("default (no match)", theme::dim()));
            } else {
                for (i, m) in snap.matched.iter().enumerate() {
                    if i > 0 {
                        row.push(Span::raw("  "));
                    }
                    row.push(Span::styled(format!("‹{m}›"), theme::accent_soft()));
                }
            }
            row.push(Span::styled("   ·   ", theme::faint()));
            row.push(Span::styled(
                format!("{} applied", snap.applied_count),
                theme::dim(),
            ));
            lines.push(Line::from(row));
        }
        lines.push(Line::from(""));

        // ---- NEXT PRIME ----
        lines.push(section_header("NEXT PRIME"));
        let mut upcoming: Vec<(time::OffsetDateTime, &str)> = snap
            .priming
            .iter()
            .filter_map(|p| p.next_fire.map(|nf| (nf, p.alias.as_str())))
            .collect();
        upcoming.sort_by_key(|(nf, _)| nf.unix_timestamp());
        match upcoming.first() {
            Some((nf, alias)) => {
                // Hero: <alias>  in <countdown> (countdown in bold accent).
                let rel = format_duration_short((nf.unix_timestamp() - now_ms / 1000) * 1000)
                    .unwrap_or_else(|| "soon".into());
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{alias}   "), theme::text()),
                    Span::styled("in ", theme::dim()),
                    Span::styled(rel, theme::accent().add_modifier(Modifier::BOLD)),
                ]));
                lines.push(Line::from(""));
                // Detail: HH:MM daily, plus the one after it, dimmed.
                let mut detail = format!("{:02}:{:02} daily", nf.hour(), nf.minute());
                if let Some((nf2, alias2)) = upcoming.get(1) {
                    detail.push_str(&format!(
                        "   ·   then {alias2} {:02}:{:02}",
                        nf2.hour(),
                        nf2.minute()
                    ));
                }
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(detail, theme::faint()),
                ]));
            }
            None => lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("no schedule", theme::dim()),
            ])),
        }

        lines
    }
}

/// A dim, outdented section header (e.g. `ACCOUNT`); content is indented beneath.
fn section_header(label: &str) -> Line<'static> {
    Line::from(Span::styled(label.to_string(), theme::dim()))
}

/// An indented `head · value` content row (head in text, dim-dot separator).
fn value_line(head: &str, value: &str, value_style: Style) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(head.to_string(), theme::text()),
        Span::styled("  ·  ", theme::faint()),
        Span::styled(value.to_string(), value_style),
    ])
}

/// One aligned gauge row: `label` (dim), a framed fine bar (accent fill on a
/// faint track, thin end caps), then `value` in `value_style`.
fn bar_line(label: &str, fraction: f64, value: &str, value_style: Style) -> Line<'static> {
    const BAR_W: usize = 18;
    let (filled, empty) = fine_bar(fraction, BAR_W);
    let mut spans = vec![
        Span::styled(label.to_string(), theme::dim()),
        Span::styled("▏", theme::faint()),
    ];
    if !filled.is_empty() {
        spans.push(Span::styled(filled, theme::accent()));
    }
    if !empty.is_empty() {
        spans.push(Span::styled(empty, theme::faint()));
    }
    spans.push(Span::styled("▕", theme::faint()));
    spans.push(Span::styled(format!("  {value}"), value_style));
    Line::from(spans)
}

impl View for Overview {
    fn title(&self) -> &str {
        "Overview"
    }

    fn footer_hints(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }

    fn on_key(&mut self, _key: KeyEvent, _ctx: &AppCtx, _snap: &Snapshot) -> Option<Action> {
        None
    }

    fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        snap: &Snapshot,
        now_ms: i64,
        _now_local: time::OffsetDateTime,
    ) {
        // Render prose directly into area — no border block.
        let body = Paragraph::new(self.lines(snap, now_ms)).style(theme::text());
        f.render_widget(body, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::snapshot::{AcctRow, PrimeRow, Snapshot};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const NOW_MS: i64 = 1_000_000 * 1000; // 1e6 seconds, in ms

    fn now_local_at(secs: i64) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap()
    }

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn snap_with_active() -> Snapshot {
        Snapshot {
            accounts: vec![AcctRow {
                alias: "work".into(),
                email: "a@b.com".into(),
                org: Some("acme".into()),
                is_active: true,
                expires_at_ms: Some(NOW_MS + 3 * 3_600_000), // 3h of token left
                has_refresh: true,
                window_start_epoch: Some(1_000_000), // window opened "now" -> ~5h left
            }],
            cwd: std::path::PathBuf::from("/tmp/cc-loadout"),
            profiles_json_exists: false,
            matched: vec![],
            applied_count: 0,
            priming: vec![PrimeRow {
                alias: "work".into(),
                next_fire: None,
                last_primed: None,
            }],
            schedule: Default::default(),
            global_enabled: Vec::new(),
            tasks: Vec::new(),
            schedule_drift: false,
            scope_drift: Vec::new(),
            maintenance: Default::default(),
        }
    }

    #[test]
    fn overview_footer_advertises_only_working_keys() {
        let v = Overview::new();
        // Overview is a read-only landing view; it must not advertise nav keys
        // it does not handle.
        for (k, _label) in v.footer_hints() {
            assert!(
                k != "↑↓" && k != "⏎",
                "Overview must not advertise unhandled nav keys, found {k}"
            );
        }
    }

    #[test]
    fn overview_renders_active_account_and_window() {
        let backend = TestBackend::new(70, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let ov = Overview::new();
        let snap = snap_with_active();
        terminal
            .draw(|f| ov.render(f, f.area(), &snap, NOW_MS, now_local_at(NOW_MS / 1000)))
            .unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("● work"), "active marker + alias: {text}");
        assert!(text.contains("acme"), "org shown");
        assert!(text.contains("ok 3h"), "token countdown shown");
        assert!(text.contains("cc-loadout"), "cwd basename shown");
        assert!(text.contains("no schedule"), "next prime placeholder");
        // Fine-bar filled cells appear (window open → filled blocks present).
        assert!(text.contains('█'), "fine bar filled chars present");
    }
}
