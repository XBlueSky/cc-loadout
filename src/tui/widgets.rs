//! Pure display helpers for the hub — no I/O, fully unit-tested.

use crate::account::format_duration_short;

/// Token status label + whether it is *urgent* (render in the alert colour):
/// expired-without-refresh, or under 1h remaining, are urgent. The word matches
/// the colour, so "ok" never shows in red — under 1h it becomes "expiring".
pub fn token_label(expires_at_ms: Option<i64>, has_refresh: bool, now_ms: i64) -> (String, bool) {
    match expires_at_ms {
        None => ("unknown".to_string(), false),
        Some(exp) => {
            let rem = exp - now_ms;
            if rem <= 0 {
                if has_refresh {
                    ("refreshable".to_string(), false)
                } else {
                    ("expired".to_string(), true)
                }
            } else if rem <= 3_600_000 {
                (
                    format!(
                        "expiring {}",
                        format_duration_short(rem).unwrap_or_default()
                    ),
                    true,
                )
            } else {
                (
                    format!("ok {}", format_duration_short(rem).unwrap_or_default()),
                    false,
                )
            }
        }
    }
}

/// The later of two optional epoch timestamps.
pub fn latest(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_picks_the_bigger_or_the_present_one() {
        assert_eq!(latest(Some(10), Some(20)), Some(20));
        assert_eq!(latest(Some(10), None), Some(10));
        assert_eq!(latest(None, Some(7)), Some(7));
        assert_eq!(latest(None, None), None);
    }

    #[test]
    fn token_label_word_matches_urgency() {
        let now = 1_000_000_000;
        // > 1h: ok, not urgent.
        let (s, urgent) = token_label(Some(now + 3 * 3_600_000), true, now);
        assert!(s.starts_with("ok "), "got {s}");
        assert!(!urgent);
        // < 1h: expiring, urgent.
        let (s, urgent) = token_label(Some(now + 7 * 60_000), true, now);
        assert!(s.starts_with("expiring "), "got {s}");
        assert!(urgent);
        // expired, no refresh: expired, urgent.
        assert_eq!(
            token_label(Some(now - 1), false, now),
            ("expired".into(), true)
        );
        // expired, has refresh: refreshable, not urgent.
        assert_eq!(
            token_label(Some(now - 1), true, now),
            ("refreshable".into(), false)
        );
    }
}

/// The Claude usage window length: 5 hours, in milliseconds.
pub const WINDOW_MS: i64 = 5 * 60 * 60 * 1000;

/// Milliseconds left in the 5h window that started at `start_epoch` (seconds),
/// relative to `now_ms`. `None` if there is no anchor or the window has closed.
pub fn window_remaining_ms(start_epoch: Option<i64>, now_ms: i64) -> Option<i64> {
    let end = start_epoch? * 1000 + WINDOW_MS;
    let rem = end - now_ms;
    (rem > 0).then_some(rem)
}

/// A two-tone bar `width` cells wide, filled to `fraction` (clamped 0.0..=1.0)
/// rounded to whole cells. Returns `(filled, empty)` as solid `█` runs so the
/// caller can colour the filled run (accent) and the track (faint): the result
/// is a clean colour boundary with no sub-cell notch. Exact values live in the
/// adjacent label, so whole-cell precision is enough.
pub fn fine_bar(fraction: f64, width: usize) -> (String, String) {
    let fraction = fraction.clamp(0.0, 1.0);
    let filled = ((fraction * width as f64).round() as usize).min(width);
    ("█".repeat(filled), "█".repeat(width - filled))
}

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState};
use ratatui::Frame;

/// Render pre-styled `lines` into `area` as a vertically-scrolling list that
/// keeps line index `cursor` visible. Selection styling is expected to be
/// already baked into the spans (each caller patches `theme::selection()` on the
/// selected row); this helper only supplies `ListState`-driven scrolling so a
/// cursor past the fold does not vanish — mirroring the accounts/tasks/schedule
/// idiom. `cursor` is a LINE index into `lines`; out-of-range is clamped.
pub fn render_scrolling_lines(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, cursor: usize) {
    let mut state = ListState::default();
    if !lines.is_empty() {
        state.select(Some(cursor.min(lines.len() - 1)));
    }
    // No highlight_style/symbol: the selected row is already styled by the
    // caller. We only want ListState's scroll-to-keep-visible behaviour, so the
    // rows never shift horizontally.
    let list =
        List::new(lines.into_iter().map(ListItem::new)).highlight_spacing(HighlightSpacing::Never);
    f.render_stateful_widget(list, area, &mut state);
}

/// A rectangle `percent_x` × `percent_y` of `area`, centered — for modal overlays.
pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vert = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vert[1])[1]
}

#[cfg(test)]
mod centered_tests {
    use super::*;

    #[test]
    fn centered_rect_is_inside_and_smaller() {
        let area = Rect::new(0, 0, 100, 40);
        let c = centered_rect(50, 30, area);
        assert!(c.width < area.width && c.height < area.height);
        assert!(c.x > 0 && c.y > 0);
    }
}

#[cfg(test)]
mod scrolling_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        t.backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn twenty_rows() -> Vec<Line<'static>> {
        (0..20).map(|i| Line::from(format!("R{i:02}"))).collect()
    }

    #[test]
    fn cursor_at_end_scrolls_last_row_into_view() {
        // 20 rows into a 5-tall viewport: the last row must be visible and the
        // first row scrolled off — the reported bug is that the last row never
        // appears no matter where the cursor is.
        let mut t = Terminal::new(TestBackend::new(10, 5)).unwrap();
        t.draw(|f| render_scrolling_lines(f, f.area(), twenty_rows(), 19))
            .unwrap();
        let text = buffer_text(&t);
        assert!(text.contains("R19"), "cursor row must be visible: {text:?}");
        assert!(
            !text.contains("R00"),
            "far-away first row must be scrolled off: {text:?}"
        );
    }

    #[test]
    fn cursor_at_start_shows_first_row_not_last() {
        let mut t = Terminal::new(TestBackend::new(10, 5)).unwrap();
        t.draw(|f| render_scrolling_lines(f, f.area(), twenty_rows(), 0))
            .unwrap();
        let text = buffer_text(&t);
        assert!(text.contains("R00"), "first row must be visible: {text:?}");
        assert!(
            !text.contains("R19"),
            "last row must be below the fold: {text:?}"
        );
    }

    #[test]
    fn short_list_renders_every_row() {
        // Fewer rows than height: no scrolling, all visible.
        let mut t = Terminal::new(TestBackend::new(10, 5)).unwrap();
        let lines = vec![Line::from("R00"), Line::from("R01")];
        t.draw(|f| render_scrolling_lines(f, f.area(), lines, 1))
            .unwrap();
        let text = buffer_text(&t);
        assert!(text.contains("R00") && text.contains("R01"), "{text:?}");
    }
}

#[cfg(test)]
mod gauge_tests {
    use super::*;

    #[test]
    fn remaining_full_at_window_start() {
        // window starts at t=1000s -> now = 1000*1000 ms -> ~full window left.
        let now_ms = 1000 * 1000;
        assert_eq!(window_remaining_ms(Some(1000), now_ms), Some(WINDOW_MS));
    }

    #[test]
    fn remaining_none_when_closed_or_unanchored() {
        let now_ms = 1000 * 1000 + WINDOW_MS + 1;
        assert_eq!(window_remaining_ms(Some(1000), now_ms), None);
        assert_eq!(window_remaining_ms(None, 0), None);
    }

    #[test]
    fn fine_bar_full_empty_and_partial() {
        assert_eq!(fine_bar(1.0, 4), ("████".to_string(), String::new()));
        assert_eq!(fine_bar(0.0, 4), (String::new(), "████".to_string()));
        assert_eq!(fine_bar(0.5, 4), ("██".to_string(), "██".to_string()));
        // rounds to whole cells: 0.6 * 5 = 3.0 filled, 2 track.
        assert_eq!(fine_bar(0.6, 5), ("███".to_string(), "██".to_string()));
        // clamps above 1.0.
        assert_eq!(fine_bar(2.0, 3), ("███".to_string(), String::new()));
    }
}
