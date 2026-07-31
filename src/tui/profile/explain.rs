//! The explain (`?`) overlay: for one scanned repo, show which profiles it
//! matches across the whole config (with provenance) and, for the profile being
//! edited, a per-rule pass/fail breakdown using the live-edited rules. This is a
//! Detail-level concern (it needs the full `Profiles`), so `DetailState` owns the
//! overlay state; `RulesState` stays a single-`Detect` editor.

use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::profile::config::{Detect, Profile, Profiles};
use crate::profile::detect::MatchReason;
use crate::profile::discover::RepoSignal;
use crate::profile::signal_detect::{self, ProfileAnswer};
use crate::tui::profile::rules::{flatten, RuleRow};
use crate::tui::theme;

/// Result of explaining one repo against the config.
pub struct ExplainReport {
    /// Profiles this repo currently matches, with a short "rule → value" string.
    pub matched_profiles: Vec<(String, String)>,
    /// True when at least one profile in `working` (not necessarily the one
    /// being edited) has a rule this repo's index can't yet answer — the
    /// "currently matches" list above may be missing an entry.
    pub cross_pending: bool,
    /// Whether the profile being edited matches this repo at all.
    pub overall: bool,
    /// Per-rule breakdown for the edited profile: (human label+value, fires?).
    pub this_rules: Vec<(String, bool)>,
    /// True when the edited profile's OWN rules couldn't be fully answered
    /// from the index (a referenced atom was never scanned) — `this_rules`
    /// is not a decisive verdict and must not be rendered as one.
    pub this_pending: bool,
}

/// Explain `sig` against `working`, with the edited profile's rules taken
/// from `this_detect` (the live, possibly-unsaved edits) rather than the saved
/// copy in `working`. Zero disk I/O: evaluated purely from the indexed
/// `RepoSignal` via `signal_detect`.
pub fn explain_repo(
    sig: &RepoSignal,
    this_name: &str,
    this_detect: &Detect,
    working: &Profiles,
) -> ExplainReport {
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
    let (explained, cross_pending) = signal_detect::detect_from_signal_explained(sig, &probe);

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

    // Resolve THIS profile's own answer directly (Match/NoMatch/Unknown) —
    // more precise than searching `explained`, which can't distinguish "no
    // match" from "not yet indexed" (an Unknown answer never lands there).
    let this_answer = signal_detect::profile_answer(sig, &probe.profiles[this_name]);
    let (this_reason, this_pending) = match this_answer {
        ProfileAnswer::Match(reason) => (Some(reason), false),
        ProfileAnswer::NoMatch => (None, false),
        ProfileAnswer::Unknown => (None, true),
    };

    let overall = this_reason.is_some();

    // Per-rule breakdown: a row fires iff the engine attributed the match to
    // THAT specific rule (engine-attributed, not isolated-probe). This is
    // faithful to the engine's first-match short-circuit and its cross-rule
    // gating (e.g. marker_files skipping content-referenced files).
    let this_rules = flatten(this_detect)
        .into_iter()
        .map(|row| {
            let label = format!("{}  {}", row.label(), row.value());
            let fires = this_reason
                .as_ref()
                .is_some_and(|reason| row_matches_reason(&row, reason));
            (label, fires)
        })
        .collect();

    ExplainReport {
        matched_profiles,
        cross_pending,
        overall,
        this_rules,
        this_pending,
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
        // An empty list is only a decisive "no profile matches" when nothing
        // in the config is still pending an index answer — otherwise a
        // profile that would have matched may simply not be indexed yet, and
        // saying "no profile" outright would be a fabricated no-match.
        if ex.report.cross_pending {
            report.push(Line::from(Span::styled(
                "(pending — some rules not yet indexed)",
                theme::faint(),
            )));
        } else {
            report.push(Line::from(Span::styled(
                "Matches no profile",
                theme::faint(),
            )));
        }
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
    } else if ex.report.this_pending {
        "This profile's rules (pending — some rules not yet indexed)"
    } else {
        "This profile's rules (no rule fires)"
    };
    report.push(Line::from(Span::styled(head.to_string(), theme::dim())));
    if ex.report.this_pending {
        // This profile's own answer is Unknown: the disk question behind at
        // least one row was never indexed, so per-row fire/no-fire marks
        // would be fabricated. Say so honestly instead of drawing dashes.
        report.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("… pending index", theme::faint()),
        ]));
    } else {
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
    use crate::profile::signal_detect::{atom_content, atom_file, atom_glob};

    /// Hand-built signal for a repo that was never touched on disk — every
    /// test in this module proves the explain path is zero-I/O by using a
    /// nonexistent path and answering purely from `rule_hits`.
    fn sig_with(path: &str, hits: &[(&str, bool)]) -> RepoSignal {
        RepoSignal {
            path: path.to_string(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: hits.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            override_names: None,
        }
    }

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
        // Fake path, no disk: Cargo.toml is indexed present, *.vue indexed absent.
        let sig = sig_with(
            "/does/not/exist/rust-repo",
            &[
                (&atom_file("Cargo.toml"), true),
                (&atom_glob("*.vue"), false),
            ],
        );

        let working = working_two();
        let this_detect = &working.profiles["rust"].detect;
        let rep = explain_repo(&sig, "rust", this_detect, &working);

        assert!(rep.overall, "rust matches via Cargo.toml");
        assert!(!rep.this_pending, "every atom this rule needs is indexed");
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
        // Both atoms are indexed and decisively absent — a real no-match,
        // not an unindexed one.
        let sig = sig_with(
            "/does/not/exist/empty-repo",
            &[
                (&atom_file("Cargo.toml"), false),
                (&atom_glob("*.vue"), false),
            ],
        );
        let working = working_two();
        let this_detect = &working.profiles["rust"].detect;
        let rep = explain_repo(&sig, "rust", this_detect, &working);
        assert!(!rep.overall, "empty repo matches no rule");
        assert!(!rep.this_pending, "both atoms are decisively indexed");
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
        use crate::profile::config::ContentRule;

        // package.json is indexed present but does NOT contain "react".
        let sig = sig_with(
            "/does/not/exist/fe-repo",
            &[
                (&atom_file("package.json"), true),
                (&atom_content("package.json", "react"), false),
            ],
        );

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

        let rep = explain_repo(&sig, "fe", &detect, &working);

        assert!(
            !rep.overall,
            "engine must not match: package.json lacks 'react'"
        );
        assert!(!rep.this_pending, "both referenced atoms are indexed");
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

    /// Task 12 Step 1: explain over a hand-built signal (fake path, zero
    /// disk) reports provenance straight from the index — `marker_glob` with
    /// the pattern value, exactly as `detect_from_signal_explained` attributes it.
    #[test]
    fn explain_over_signal_reports_marker_glob_provenance_zero_disk() {
        let sig = sig_with("/does/not/exist/vue-repo", &[(&atom_glob("*.vue"), true)]);

        let mut profiles_map = std::collections::BTreeMap::new();
        let mut fe = Profile::default();
        fe.detect.marker_globs.push("*.vue".into());
        profiles_map.insert("frontend".to_string(), fe);
        let working = Profiles {
            profiles: profiles_map,
            ..Default::default()
        };
        let this_detect = &working.profiles["frontend"].detect;

        let rep = explain_repo(&sig, "frontend", this_detect, &working);

        assert!(rep.overall);
        assert!(!rep.this_pending);
        let (_, why) = rep
            .matched_profiles
            .iter()
            .find(|(n, _)| n == "frontend")
            .expect("frontend must be reported as a match");
        assert!(
            why.contains("marker_glob"),
            "rule name in provenance: {why}"
        );
        assert!(why.contains("*.vue"), "pattern value in provenance: {why}");
        let glob_row = rep
            .this_rules
            .iter()
            .find(|(label, _)| label.contains("*.vue"))
            .expect("*.vue row must exist");
        assert!(glob_row.1, "*.vue row must show as firing");
    }

    /// Pending case: the profile's own rule references an atom the index
    /// never scanned. The report must say so honestly, not fabricate a
    /// decisive no-match.
    #[test]
    fn explain_reports_pending_not_fabricated_no_match_when_atom_unindexed() {
        // Empty index: the *.tsx atom was never scanned for this repo.
        let sig = sig_with("/does/not/exist/pending-repo", &[]);

        let mut profiles_map = std::collections::BTreeMap::new();
        let mut fe = Profile::default();
        fe.detect.marker_globs.push("*.tsx".into());
        profiles_map.insert("frontend".to_string(), fe);
        let working = Profiles {
            profiles: profiles_map,
            ..Default::default()
        };
        let this_detect = &working.profiles["frontend"].detect;

        let rep = explain_repo(&sig, "frontend", this_detect, &working);

        assert!(!rep.overall, "an unindexed atom is not a decisive match");
        assert!(
            rep.this_pending,
            "unindexed atom must mark this profile pending, not resolved"
        );
        assert!(
            rep.cross_pending,
            "the only profile in the config is pending, so the cross-profile list is too"
        );
    }

    #[test]
    fn render_pending_shows_honest_line_not_fabricated_no_match() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let sig = sig_with("/does/not/exist/pending-repo", &[]);
        let mut profiles_map = std::collections::BTreeMap::new();
        let mut fe = Profile::default();
        fe.detect.marker_globs.push("*.tsx".into());
        profiles_map.insert("frontend".to_string(), fe);
        let working = Profiles {
            profiles: profiles_map,
            ..Default::default()
        };
        let this_detect = working.profiles["frontend"].detect.clone();
        let report = explain_repo(&sig, "frontend", &this_detect, &working);
        let ex = ExplainState {
            repos: vec![sig.path.clone()],
            cursor: 0,
            report,
        };

        let mut t = Terminal::new(TestBackend::new(90, 20)).unwrap();
        t.draw(|f| render(&ex, f, f.area())).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(
            text.contains("pending"),
            "overlay must announce the pending index honestly: {text}"
        );
        assert!(
            !text.contains("Matches no profile"),
            "must not fabricate a decisive no-match: {text}"
        );
        assert!(
            !text.contains("no rule fires"),
            "must not fabricate a decisive no-fire verdict: {text}"
        );
    }
}
