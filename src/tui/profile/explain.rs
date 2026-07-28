//! The explain (`?`) overlay: for one scanned repo, show which profiles it
//! matches across the whole config (with provenance) and, for the profile being
//! edited, a per-rule pass/fail breakdown using the live-edited rules. This is a
//! Detail-level concern (it needs the full `Profiles`), so `DetailState` owns the
//! overlay state; `RulesState` stays a single-`Detect` editor.

use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::path::Path;

use crate::profile::config::{Detect, Profile, Profiles};
use crate::profile::detect::MatchReason;
use crate::tui::profile::rules::{flatten, RuleRow};
use crate::tui::theme;

/// Result of explaining one repo against the config.
pub struct ExplainReport {
    /// Profiles this repo currently matches, with a short "rule → value" string.
    pub matched_profiles: Vec<(String, String)>,
    /// Whether the profile being edited matches this repo at all.
    pub overall: bool,
    /// Per-rule breakdown for the edited profile: (human label+value, fires?).
    pub this_rules: Vec<(String, bool)>,
}

/// Explain `repo_path` against `working`, with the edited profile's rules taken
/// from `this_detect` (the live, possibly-unsaved edits) rather than the saved
/// copy in `working`.
pub fn explain_repo(
    repo_path: &str,
    this_name: &str,
    this_detect: &Detect,
    working: &Profiles,
) -> ExplainReport {
    let path = Path::new(repo_path);

    // Cross-profile provenance: probe against the full config, but with THIS
    // profile's detect swapped to the live-edited rules so the breakdown matches
    // what the user is editing.
    let mut probe = working.clone();
    if let Some(p) = probe.profiles.get_mut(this_name) {
        p.detect = this_detect.clone();
    } else {
        probe.profiles.insert(
            this_name.to_string(),
            Profile {
                plugins: Vec::new(),
                detect: this_detect.clone(),
            },
        );
    }
    let explained = crate::profile::detect::detect_profiles_explained(path, &probe);

    // Extract the MatchReason the engine attributed to this_name (if any).
    let this_reason: Option<&MatchReason> = explained
        .iter()
        .find(|(name, _)| name == this_name)
        .map(|(_, reason)| reason);

    let matched_profiles: Vec<(String, String)> = explained
        .iter()
        .map(|(name, reason)| {
            let val = reason
                .value
                .as_ref()
                .map(|v| format!("{} → {}", reason.rule, v))
                .unwrap_or_else(|| reason.rule.to_string());
            (name.clone(), val)
        })
        .collect();

    let overall = this_reason.is_some();

    // Per-rule breakdown: a row fires iff the engine attributed the match to
    // THAT specific rule (engine-attributed, not isolated-probe). This is
    // faithful to the engine's first-match short-circuit and its cross-rule
    // gating (e.g. marker_files skipping content-referenced files).
    let this_rules = flatten(this_detect)
        .into_iter()
        .map(|row| {
            let label = format!("{}  {}", row.label(), row.value());
            let fires = this_reason.is_some_and(|reason| row_matches_reason(&row, reason));
            (label, fires)
        })
        .collect();

    ExplainReport {
        matched_profiles,
        overall,
        this_rules,
    }
}

/// True iff the engine's `MatchReason` is attributable to `row`. Maps
/// `reason.rule` (the engine's static label) to the corresponding `RuleRow`
/// variant, then compares values.
fn row_matches_reason(row: &RuleRow, reason: &MatchReason) -> bool {
    match (row, reason.rule) {
        (RuleRow::PathUnder(p), "path_prefix") => reason.value.as_deref() == Some(p.as_str()),
        (RuleRow::HasFile(f), "marker_file") => reason.value.as_deref() == Some(f.as_str()),
        (RuleRow::HasAny(g), "marker_glob") => reason.value.as_deref() == Some(g.as_str()),
        (RuleRow::Contains { .. }, "content") => {
            // The engine formats content value as "{file} → {word}" (same as
            // RuleRow::Contains::value()), so compare directly.
            reason.value.as_deref() == Some(row.value().as_str())
        }
        (RuleRow::Legacy(s), "package_json_dep") => {
            reason.value.as_deref() == s.strip_prefix("package.json: ")
        }
        (RuleRow::Legacy(s), "deps_keyword") => {
            reason.value.as_deref() == s.strip_prefix("keyword: ")
        }
        _ => false,
    }
}

/// Open explain overlay: a repo list + the report for the selected repo.
pub struct ExplainState {
    pub repos: Vec<String>,
    pub cursor: usize,
    pub report: ExplainReport,
}

/// Render the explain overlay (borderless).
///
/// Two regions: a scrolling repo selector on top (so a long repo list stays
/// navigable) and the fixed report for the selected repo below (so it never
/// scrolls out of reach as the list grows).
pub fn render(ex: &ExplainState, f: &mut Frame, area: Rect) {
    // ── Selector lines (header + blank + one row per repo) ────────────────
    let mut sel_lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "Explain — why does a repo match? (↑↓ choose, esc back)",
            theme::accent(),
        )),
        Line::raw(""),
    ];
    let cursor_line = sel_lines.len() + ex.cursor; // header + blank precede row 0
    for (i, p) in ex.repos.iter().enumerate() {
        let marker = if i == ex.cursor { "▸ " } else { "  " };
        let name = p.rsplit('/').next().unwrap_or(p);
        sel_lines.push(Line::from(vec![
            Span::styled(marker.to_string(), theme::accent()),
            Span::styled(name.to_string(), theme::text()),
        ]));
    }

    // ── Report lines (provenance + per-rule breakdown) ────────────────────
    let mut report: Vec<Line<'static>> = Vec::new();
    if ex.report.matched_profiles.is_empty() {
        report.push(Line::from(Span::styled(
            "Matches no profile",
            theme::faint(),
        )));
    } else {
        report.push(Line::from(Span::styled("Currently matches", theme::dim())));
        for (name, why) in &ex.report.matched_profiles {
            report.push(Line::from(Span::styled(
                format!("   {name}   {why}"),
                theme::text(),
            )));
        }
    }
    report.push(Line::raw(""));
    let head = if ex.report.overall {
        "This profile's rules (matches)"
    } else {
        "This profile's rules (no rule fires)"
    };
    report.push(Line::from(Span::styled(head.to_string(), theme::dim())));
    for (label, fires) in &ex.report.this_rules {
        let mark = if *fires { "●" } else { "╴" };
        let style = if *fires {
            theme::accent()
        } else {
            theme::faint()
        };
        report.push(Line::from(vec![
            Span::styled(format!("   {mark} "), style),
            Span::styled(label.clone(), theme::text()),
        ]));
    }

    // Report takes up to half the height (plus a blank separator); the selector
    // gets the rest and scrolls to keep the chosen repo visible.
    let report_h = (report.len() as u16 + 1).min(area.height / 2);
    let chunks =
        ratatui::layout::Layout::vertical([Constraint::Min(1), Constraint::Length(report_h)])
            .split(area);
    crate::tui::widgets::render_scrolling_lines(f, chunks[0], sel_lines, cursor_line);
    f.render_widget(Paragraph::new(report), chunks[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::{Profile, Profiles};

    fn working_two() -> Profiles {
        let mut p = std::collections::BTreeMap::new();
        let mut rust = Profile::default();
        rust.detect.marker_files.push("Cargo.toml".into());
        rust.detect.marker_globs.push("*.vue".into()); // present but won't fire here
        p.insert("rust".to_string(), rust);
        let mut fe = Profile::default();
        fe.detect.marker_globs.push("*.vue".into());
        p.insert("frontend".to_string(), fe);
        Profiles {
            profiles: p,
            ..Default::default()
        }
    }

    #[test]
    fn explain_reports_overall_and_per_rule_for_this_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let path = dir.path().display().to_string();

        let working = working_two();
        let this_detect = &working.profiles["rust"].detect;
        let rep = explain_repo(&path, "rust", this_detect, &working);

        assert!(rep.overall, "rust matches via Cargo.toml");
        // Cross-profile: rust matches (marker_file Cargo.toml); frontend does NOT.
        assert!(rep.matched_profiles.iter().any(|(n, _)| n == "rust"));
        assert!(!rep.matched_profiles.iter().any(|(n, _)| n == "frontend"));
        // Per-rule: has file Cargo.toml fires; has any *.vue does not.
        let has_file = rep
            .this_rules
            .iter()
            .find(|(label, _)| label.contains("Cargo.toml"))
            .unwrap();
        assert!(has_file.1, "Cargo.toml rule fires");
        let has_vue = rep
            .this_rules
            .iter()
            .find(|(label, _)| label.contains("*.vue"))
            .unwrap();
        assert!(!has_vue.1, "*.vue rule does not fire (no .vue file)");
    }

    #[test]
    fn explain_overall_false_when_nothing_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().display().to_string();
        let working = working_two();
        let this_detect = &working.profiles["rust"].detect;
        let rep = explain_repo(&path, "rust", this_detect, &working);
        assert!(!rep.overall, "empty repo matches no rule");
        assert!(rep.this_rules.iter().all(|(_, fires)| !fires));
    }

    /// M1 regression: a profile with marker_files=[package.json] + content=[{package.json,
    /// react}] against a repo that has package.json WITHOUT "react" must show
    /// overall=false AND the package.json has-file row must NOT show as firing.
    ///
    /// The old isolated-probe code would show the HasFile row as firing because
    /// it probed a one-rule config (marker_files=[package.json]) and that single
    /// rule matched on existence — ignoring the engine's gate that skips
    /// marker_files entries referenced by content rules.  The engine-attributed
    /// fix resolves this: no MatchReason is returned for this_name, so no row fires.
    #[test]
    fn explain_does_not_show_gated_marker_as_firing() {
        use crate::profile::config::{ContentRule, Profile, Profiles};

        let dir = tempfile::tempdir().unwrap();
        // package.json exists but does NOT contain "react".
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        let path = dir.path().display().to_string();

        let mut detect = Detect::default();
        detect.marker_files.push("package.json".into());
        detect.content.push(ContentRule {
            file: "package.json".into(),
            word: "react".into(),
        });

        let mut profiles_map = std::collections::BTreeMap::new();
        profiles_map.insert(
            "fe".to_string(),
            Profile {
                plugins: vec![],
                detect: detect.clone(),
            },
        );
        let working = Profiles {
            profiles: profiles_map,
            ..Default::default()
        };

        let rep = explain_repo(&path, "fe", &detect, &working);

        assert!(
            !rep.overall,
            "engine must not match: package.json lacks 'react'"
        );
        // The HasFile(package.json) row must NOT show as firing.
        let has_file_row = rep
            .this_rules
            .iter()
            .find(|(label, _)| label.contains("package.json") && label.contains("has file"))
            .expect("has file row must exist");
        assert!(
            !has_file_row.1,
            "gated marker_file must NOT show as firing (overall=false, no rule attributed)"
        );
    }
}
