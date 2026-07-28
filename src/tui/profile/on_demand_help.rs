//! Static help overlay for the On-demand board row: what the bucket is,
//! how a plugin gets into it, and how to acquire/release one. Borrows the
//! explain overlay's borderless look (accent title, plain body); holds no
//! state — the content is fixed, so `render` is a free function.
//! See `docs/superpowers/specs/2026-07-22-on-demand-discoverability-design.md`.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::theme;

/// The overlay's full content. Factored out of `render` so tests can assert
/// on the text without a terminal.
fn lines() -> Vec<Line<'static>> {
    vec![
        Line::from(Span::styled(
            "On-demand plugins — press esc to close",
            theme::accent(),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "Plugins you want only sometimes, in one repo, for one task —",
            theme::text(),
        )),
        Line::from(Span::styled(
            "not standing in every repo (universal) and not tied to any",
            theme::text(),
        )),
        Line::from(Span::styled(
            "repo signal (a profile). They live in a separate bucket that",
            theme::text(),
        )),
        Line::from(Span::styled("`apply` never force-manages.", theme::text())),
        Line::raw(""),
        Line::from(vec![
            Span::styled("Add one:   ", theme::dim()),
            Span::styled("edit a profile → Assign → On-demand", theme::text()),
        ]),
        Line::from(vec![
            Span::styled("Use one:   ", theme::dim()),
            Span::styled("/cc-loadout:acquire <key>", theme::accent_soft()),
            Span::styled("   then   ", theme::dim()),
            Span::styled("/reload-plugins", theme::accent_soft()),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "It auto-reverts when this session ends. Return it early with",
            theme::text(),
        )),
        Line::from(vec![
            Span::styled("/cc-loadout:release <key>", theme::accent_soft()),
            Span::styled(". A stuck hold: ", theme::text()),
            Span::styled("release --force <key>", theme::accent_soft()),
            Span::styled(".", theme::text()),
        ]),
    ]
}

/// Render the overlay (borderless, fills `area`).
pub(crate) fn render(f: &mut Frame, area: Rect) {
    f.render_widget(Paragraph::new(lines()), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat() -> String {
        lines()
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn overlay_names_the_acquire_flow_and_the_close_key() {
        let text = flat();
        assert!(
            text.contains("/cc-loadout:acquire"),
            "must point at acquire: {text}"
        );
        assert!(
            text.contains("/reload-plugins"),
            "must mention the reload step: {text}"
        );
        assert!(text.contains("esc"), "must say how to close: {text}");
        assert!(
            text.contains("auto-reverts"),
            "must explain session scoping: {text}"
        );
        assert!(
            text.contains("Assign"),
            "must say how a plugin enters the bucket: {text}"
        );
    }
}
