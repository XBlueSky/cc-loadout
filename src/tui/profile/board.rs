use std::collections::BTreeSet;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::profile::config::Detect;
use crate::profile::draft::unassigned_keys;
use crate::tui::snapshot::Snapshot;
use crate::tui::theme;

use super::ProfileView;

/// Render the profile board, with drift badges when there are items to review.
///
/// Board structure (when review_count > 0):
///   ● {n} to review                    ← accent header
///   ⚠ {k} repos match nothing          ← only when uncovered non-empty
///   ⚠ global out of sync ({n})         ← only when global non-empty
///                                       ← blank separator
///   <rows…>
///
/// When review_count == 0 the header is entirely omitted (clean board).
pub fn render(view: &ProfileView, snap: &Snapshot, f: &mut Frame, area: Rect) {
    // ── Assemble drift ───────────────────────────────────────────────────
    let d = crate::profile::drift::Drift {
        new_unassigned: crate::profile::draft::unassigned_keys(&view.inv, &view.working),
        stale: crate::profile::drift::stale_refs(&view.inv, &view.working),
        uncovered: view.uncovered.clone(),
        global: crate::profile::drift::global_drift(&view.working, &snap.global_enabled),
    };
    let stale: BTreeSet<&str> = d.stale.iter().map(String::as_str).collect();

    let mut lines: Vec<Line<'static>> = Vec::new();
    // Line index of the selected row. The AI-offer and drift header push a
    // variable number of leading lines, so the row index (`view.cursor`) is not
    // the line index the scroller needs.
    let mut cursor_line = 0usize;

    // ── AI consent offer (first-run, claude available) ────────────────────
    if view.ai_offer {
        lines.push(Line::from(Span::styled(
            "Let Claude draft your profiles?  [y] yes  [n] no  (uses your account)",
            theme::accent_soft(),
        )));
    }

    // ── Drift header (only when there is something to review) ────────────
    if d.review_count() > 0 {
        let n = d.review_count();
        lines.push(Line::from(Span::styled(
            format!("● {n} to review"),
            theme::accent(),
        )));
        if !d.uncovered.is_empty() {
            let k = d.uncovered.len();
            lines.push(Line::from(Span::styled(
                format!("⚠ {k} repos match nothing"),
                theme::alert(),
            )));
        }
        if !d.global.is_empty() {
            let g = d.global.len();
            lines.push(Line::from(Span::styled(
                format!("⚠ global out of sync ({g})"),
                theme::alert(),
            )));
        }
        // blank separator between header and rows
        lines.push(Line::from(""));
    }

    // ── Universal row ────────────────────────────────────────────────────
    let univ_plugins = if view.working.universal.is_empty() {
        "(none)".to_string()
    } else {
        view.working.universal.join(", ")
    };
    let univ_stale = view
        .working
        .universal
        .iter()
        .any(|p| stale.contains(p.as_str()));
    if view.cursor == 0 {
        cursor_line = lines.len();
    }
    lines.push(build_row(
        "Universal",
        &univ_plugins,
        "every repo",
        view.cursor == 0,
        univ_stale,
    ));

    // ── Per-profile rows ─────────────────────────────────────────────────
    for (i, (name, prof)) in view.working.profiles.iter().enumerate() {
        let row_idx = i + 1; // Universal is index 0
        let plugins = if prof.plugins.is_empty() {
            "(universal only)".to_string()
        } else {
            prof.plugins.join(", ")
        };
        let detect = detect_summary(&prof.detect);
        let has_stale = prof.plugins.iter().any(|p| stale.contains(p.as_str()));
        if view.cursor == row_idx {
            cursor_line = lines.len();
        }
        lines.push(build_row(
            name,
            &plugins,
            &detect,
            view.cursor == row_idx,
            has_stale,
        ));
    }

    // ── Unassigned row ───────────────────────────────────────────────────
    let unassigned_idx = view.working.profiles.len() + 1;
    let unassigned = unassigned_keys(&view.inv, &view.working);
    let unassigned_label = if !d.new_unassigned.is_empty() {
        "Unassigned ●".to_string()
    } else {
        "Unassigned".to_string()
    };
    let unassigned_str = if unassigned.is_empty() {
        "(none)".to_string()
    } else {
        unassigned.join(", ")
    };
    // Unassigned row never shows the stale marker (stale keys are in profile rows)
    if view.cursor == unassigned_idx {
        cursor_line = lines.len();
    }
    lines.push(build_row(
        &unassigned_label,
        &unassigned_str,
        "",
        view.cursor == unassigned_idx,
        false,
    ));

    // ── On-demand row ────────────────────────────────────────────────────
    let on_demand_idx = view.working.profiles.len() + 2;
    let on_demand_str = if view.working.on_demand.is_empty() {
        "(none)".to_string()
    } else {
        view.working.on_demand.join(", ")
    };
    if view.cursor == on_demand_idx {
        cursor_line = lines.len();
    }
    let mut on_demand_row = build_row(
        "On-demand",
        &on_demand_str,
        "",
        view.cursor == on_demand_idx,
        false,
    );
    // Discoverability cue: this row alone carries a quiet trailing `?`.
    // The footer advertises `? what is on-demand?` once the row is selected.
    on_demand_row.spans.push(Span::styled("?", theme::dim()));
    lines.push(on_demand_row);

    crate::tui::widgets::render_scrolling_lines(f, area, lines, cursor_line);
}

fn build_row(
    label: &str,
    plugins: &str,
    detect: &str,
    selected: bool,
    stale: bool,
) -> Line<'static> {
    let label_style = if selected {
        theme::selection().patch(theme::accent_soft())
    } else {
        theme::accent_soft()
    };
    let mid_style = if selected {
        theme::selection().patch(theme::text())
    } else {
        theme::text()
    };
    let detect_style = if selected {
        theme::selection().patch(theme::dim())
    } else {
        theme::dim()
    };

    let mut spans = vec![
        Span::styled(format!("{label:<14} "), label_style),
        Span::styled(format!("{plugins:<30} "), mid_style),
    ];
    if !detect.is_empty() {
        spans.push(Span::styled(detect.to_string(), detect_style));
    }
    if stale {
        spans.push(Span::styled(" ⚠ not installed".to_string(), theme::dim()));
    }
    Line::from(spans)
}

/// A short, human description of what a profile matches on.
fn detect_summary(d: &Detect) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(d.path_prefixes.iter().cloned());
    parts.extend(d.marker_files.iter().cloned());
    parts.extend(d.marker_globs.iter().cloned());
    parts.extend(d.content.iter().map(|c| format!("{}→{}", c.file, c.word)));
    if parts.is_empty() {
        "any repo".to_string()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::detect_summary;
    use crate::profile::config::Detect;

    #[test]
    fn detect_summary_always_shows_path_prefixes() {
        let mut d = Detect::default();
        d.path_prefixes.push("/workspace/".into());
        d.marker_files.push("Cargo.toml".into());
        let s = detect_summary(&d);
        assert!(
            s.contains("/workspace/"),
            "path prefix must show even alongside other rules: {s}"
        );
        assert!(s.contains("Cargo.toml"), "marker file still shown: {s}");
    }

    #[test]
    fn on_demand_row_carries_help_cue_and_others_do_not() {
        use crate::profile::config::Profiles;
        use crate::profile::discover::Inventory;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let inv = Inventory {
            plugins: vec![],
            repos: vec![],
            suggested_profiles: vec![],
        };
        // claude_available=false, offer=false → no AI-offer line (which
        // contains a literal '?') can pollute the buffer.
        let view = super::ProfileView::new(inv, Profiles::default(), false, false);
        let snap = crate::tui::profile::test_support::snap();

        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| super::render(&view, &snap, f, f.area()))
            .unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        let od = text.find("On-demand").expect("On-demand row must render");
        assert!(
            text[od..].contains('?'),
            "On-demand row must carry the ? cue; got: {text}"
        );
        let univ = text.find("Universal").expect("Universal row must render");
        assert!(
            !text[univ..od].contains('?'),
            "rows before On-demand must not carry ?; got: {text}"
        );
    }
}
