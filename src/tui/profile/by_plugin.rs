//! By-plugin view: a flat list of ALL installed plugins, each row showing its
//! membership, with the selected plugin's description shown in a pane below.
//!
//! Toggled against the by-profile board with `v`.  `⏎` on a plugin opens the
//! [`MembershipPick`] picker (Task 4).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::profile::config::Profiles;
use crate::tui::theme;

use super::ProfileView;

/// The sentinel shown at the end of the picker's target list.
pub(super) const NEW_PROFILE: &str = "+ New profile\u{2026}";

/// A scan older than this reads as possibly stale — the bar warns and suggests s.
const SCAN_STALE_SECS: i64 = 7 * 24 * 60 * 60;

// ── MembershipPick ────────────────────────────────────────────────────────────

/// Transient state for the multi-profile membership picker.
///
/// Opened when the user presses `⏎` on a plugin in the ByPlugin board.
/// Targets: `["Universal"] ++ working.profiles.keys() ++ ["+ New profile…"]`.
/// `checked` is parallel to `targets`.
pub struct MembershipPick {
    /// The plugin key being re-assigned.
    pub key: String,
    /// The picker options (labels).
    pub targets: Vec<String>,
    /// Checked state parallel to `targets`.  `"+ New profile…"` is never pre-checked.
    pub checked: Vec<bool>,
    /// Cursor position within `targets`.
    pub cursor: usize,
    /// Active while the user is typing a new profile name.
    pub naming: Option<crate::tui::textinput::TextInput>,
}

impl MembershipPick {
    /// Build a picker for `key` initialised from the current working config.
    pub fn open(working: &Profiles, key: &str) -> Self {
        let mut targets = vec!["Universal".to_string()];
        targets.extend(working.profiles.keys().cloned());
        targets.push(NEW_PROFILE.to_string());

        let mem = super::by_plugin::membership(working, key);
        let checked: Vec<bool> = targets
            .iter()
            .map(|t| {
                if t == NEW_PROFILE {
                    false
                } else if t == "Universal" {
                    mem == vec!["Universal".to_string()]
                } else {
                    mem.contains(t)
                }
            })
            .collect();

        MembershipPick {
            key: key.to_string(),
            targets,
            checked,
            cursor: 0,
            naming: None,
        }
    }
}

/// Compute the membership of a plugin key in the given working config.
///
/// Returns:
/// - `["Universal"]` if `key` is in `working.universal`.
/// - `["On-demand"]` if `key` is in `working.on_demand`.
/// - The profile names (BTreeMap order) whose `plugins` contain `key`.
/// - `[]` if the key is in none of the above buckets.
pub fn membership(working: &Profiles, key: &str) -> Vec<String> {
    if working.universal.iter().any(|k| k == key) {
        return vec!["Universal".to_string()];
    }
    // on_demand and profiles are disjoint pools by design (see the
    // 2026-07-03 on-demand spec), so short-circuit ordering is safe.
    if working.on_demand.iter().any(|k| k == key) {
        return vec!["On-demand".to_string()];
    }
    let mut names: Vec<String> = working
        .profiles
        .iter()
        .filter(|(_, prof)| prof.plugins.iter().any(|k| k == key))
        .map(|(name, _)| name.clone())
        .collect();
    // BTreeMap iteration is already sorted, but collect preserves that order.
    names.sort(); // defensive — BTreeMap order is already sorted
    names
}

/// Render the by-plugin view into `area`.
///
/// Layout (vertical):
///   3 lines — header: a centered scan bar, the "All plugins (n)" title, a breather
///   body    — scrollable plugin list (fills remaining minus desc pane)
///   3 lines — description pane for the selected plugin
pub fn render(view: &ProfileView, f: &mut Frame, area: Rect, now_ms: i64) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // header: title + centered scan bar + breather
        Constraint::Min(1),    // plugin list
        Constraint::Length(3), // description pane
    ])
    .split(area);

    let header_area = chunks[0];
    let list_area = chunks[1];
    let desc_area = chunks[2];

    // ── Header ───────────────────────────────────────────────────────────────
    // Line 0: a CENTERED, prominent scan bar — the explicit-scan affordance,
    //   placed ABOVE the title so it is the first thing seen. The action keys
    //   (s / r) are accent-coloured so they read as actionable; the path is dim
    //   and left-elided so a long root never overflows.
    // Line 1: the "All plugins (n)" title (left).
    // Line 2: a blank breather before the list.
    let n = view.inv.plugins.len();
    let scanned = view.inv.repos.len();
    let nroots = view.scan_roots.len();
    let root_summary = if nroots == 0 {
        "no scan root".to_string()
    } else if nroots == 1 {
        elide_left(&view.scan_roots[0], 36)
    } else {
        format!("{nroots} roots")
    };

    let title = Line::from(Span::styled(
        format!("All plugins ({n})"),
        theme::accent_soft(),
    ));

    let sep = || Span::styled("   \u{b7}   ", theme::faint());
    let mut bar: Vec<Span<'static>> = Vec::new();
    if scanned == 0 {
        bar.push(Span::styled("scan: ", theme::dim()));
        bar.push(Span::styled(root_summary, theme::dim()));
    } else {
        let now_secs = now_ms / 1000;
        let stale = view
            .scanned_at
            .map(|t| now_secs - t > SCAN_STALE_SECS)
            .unwrap_or(false);
        let count_style = if stale { theme::alert() } else { theme::dim() };
        bar.push(Span::styled(
            format!("{root_summary}  \u{b7}  "),
            theme::dim(),
        ));
        bar.push(Span::styled(format!("{scanned} repos"), count_style));
        if let Some(t) = view.scanned_at {
            let rel = super::fmt_age(t, now_secs);
            let tail = if stale {
                format!("  \u{b7}  {rel} \u{b7} may be stale")
            } else {
                format!("  \u{b7}  scanned {rel}")
            };
            bar.push(Span::styled(tail, count_style));
        }
    }
    bar.push(sep());
    bar.push(Span::styled("s", theme::accent()));
    bar.push(Span::styled(
        if scanned == 0 { " scan" } else { " rescan" },
        theme::dim(),
    ));
    bar.push(sep());
    bar.push(Span::styled("r", theme::accent()));
    bar.push(Span::styled(" roots", theme::dim()));
    if view.indexing {
        bar.push(sep());
        let tail = match view.indexing_atoms.as_slice() {
            [atom] => format!("indexing {atom}\u{2026}"),
            atoms => format!("indexing {} patterns\u{2026}", atoms.len()),
        };
        bar.push(Span::styled(tail, theme::dim()));
    }

    let bar_w: usize = bar.iter().map(|s| s.content.chars().count()).sum();
    let pad = (header_area.width as usize).saturating_sub(bar_w) / 2;
    let mut bar_line: Vec<Span<'static>> = vec![Span::raw(" ".repeat(pad))];
    bar_line.extend(bar);

    f.render_widget(
        Paragraph::new(vec![Line::from(bar_line), title, Line::from("")]),
        header_area,
    );

    // ── Plugin list ───────────────────────────────────────────────────────────
    let mut list_lines: Vec<Line<'static>> = Vec::with_capacity(n);
    for (i, plugin) in view.inv.plugins.iter().enumerate() {
        let selected = i == view.plugin_cursor;
        let members = membership(&view.working, &plugin.key);
        let membership_str = if members.is_empty() {
            "—".to_string()
        } else {
            members.join(" · ")
        };

        let key_style = if selected {
            theme::selection().patch(theme::accent_soft())
        } else {
            theme::accent_soft()
        };
        let mid_style = if selected {
            theme::selection().patch(theme::dim())
        } else {
            theme::dim()
        };

        list_lines.push(Line::from(vec![
            Span::styled(format!("{:<30} ", plugin.key), key_style),
            Span::styled(format!("→ {membership_str}"), mid_style),
        ]));
    }
    // Windowed so a cursor past the fold scrolls into view instead of vanishing.
    crate::tui::widgets::render_scrolling_lines(f, list_area, list_lines, view.plugin_cursor);

    // ── Description pane ─────────────────────────────────────────────────────
    let desc_style = theme::text();

    let mut desc_lines: Vec<Line<'static>> = Vec::new();

    if let Some(plugin) = view.inv.plugins.get(view.plugin_cursor) {
        // Parse key: "<name>@<marketplace>"
        let (name_part, mkt_part) = plugin
            .key
            .split_once('@')
            .map(|(n, m)| (n.to_string(), m.to_string()))
            .unwrap_or_else(|| (plugin.key.clone(), String::new()));

        // Line 1: key + marketplace
        let key_line = if mkt_part.is_empty() {
            Line::from(Span::styled(name_part, desc_style))
        } else {
            Line::from(vec![
                Span::styled(name_part, desc_style),
                Span::styled(format!("  @{mkt_part}"), theme::faint()),
            ])
        };
        desc_lines.push(key_line);

        // Line 2: description
        let desc_text = plugin.description.as_deref().unwrap_or("(no description)");
        desc_lines.push(Line::from(Span::styled(desc_text.to_string(), desc_style)));
    }

    f.render_widget(Paragraph::new(desc_lines), desc_area);
}

// ── Picker render ─────────────────────────────────────────────────────────────

/// Render the membership picker overlay into `area`.
pub fn render_picker(pick: &MembershipPick, f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled("ASSIGN PLUGIN    ", theme::accent()),
        Span::styled(pick.key.clone(), theme::text()),
    ]));
    lines.push(Line::from(""));

    if let Some(ti) = &pick.naming {
        lines.push(Line::from(vec![
            Span::styled("name    ", theme::dim()),
            Span::styled(ti.render_line(), theme::text()),
        ]));
    } else {
        for (i, target) in pick.targets.iter().enumerate() {
            let selected = i == pick.cursor;
            let base_style = if selected {
                theme::selection().patch(theme::text())
            } else {
                theme::text()
            };
            let prefix = if selected { "\u{25b8} " } else { "  " };
            let check = if target == NEW_PROFILE {
                "   "
            } else if pick.checked[i] {
                "[x]"
            } else {
                "[ ]"
            };
            lines.push(Line::from(vec![Span::styled(
                format!("{prefix}{check} {target}"),
                base_style,
            )]));
        }
    }

    // 2 header lines (title + blank) precede the target rows.
    let cursor_line = if pick.naming.is_some() {
        0
    } else {
        2 + pick.cursor
    };
    crate::tui::widgets::render_scrolling_lines(f, area, lines, cursor_line);
}

/// The sentinel row shown at the end of the roots manager list.
pub(super) const ADD_ROOT: &str = "+ add root\u{2026}";

/// Given a partial absolute path, return the completion SUFFIX for the first
/// (alphabetical) subdirectory of its parent whose name starts with the typed
/// leaf, plus a trailing `/`. Returns `None` when there is no `/`, the parent
/// is unreadable, or nothing matches. Read-only and failure-tolerant.
pub(super) fn dir_suggestion(input: &str) -> Option<String> {
    let idx = input.rfind('/')?;
    let parent = &input[..=idx];
    let leaf = &input[idx + 1..];
    let mut names: Vec<String> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(leaf))
        .collect();
    names.sort();
    let first = names.into_iter().next()?;
    // `first` starts with `leaf` (byte prefix), so slicing at leaf.len() is safe.
    Some(format!("{}/", &first[leaf.len()..]))
}

/// Transient state for the Scan Roots manager (by-plugin Board; opened with `r`).
pub struct RootsEdit {
    /// Working copy of the scan roots.
    pub roots: Vec<String>,
    /// Cursor over `0..=roots.len()`; `roots.len()` is the `+ add root…` row.
    pub cursor: usize,
    /// `Some` while adding or editing a root (text entry).
    pub input: Option<crate::tui::textinput::TextInput>,
    /// `Some(i)` while editing `roots[i]`; `None` while adding a new root.
    pub edit_idx: Option<usize>,
    /// Cached ghost completion (suffix to append) for the current input;
    /// recomputed on input change. `None` when there is nothing to complete.
    pub suggestion: Option<String>,
}

impl RootsEdit {
    /// Open a manager seeded from the current roots.
    pub fn open(roots: &[String]) -> Self {
        RootsEdit {
            roots: roots.to_vec(),
            cursor: 0,
            input: None,
            edit_idx: None,
            suggestion: None,
        }
    }
}

/// Render the Scan Roots manager overlay into `area`.
pub fn render_roots_manager(ed: &RootsEdit, f: &mut Frame, area: Rect) {
    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled("SCAN ROOTS", theme::accent())),
        Line::from(""),
    ];

    if let Some(input) = &ed.input {
        let label = if ed.edit_idx.is_some() {
            "edit  "
        } else {
            "add   "
        };
        let mut spans = vec![
            Span::styled(label, theme::dim()),
            Span::styled(input.render_line(), theme::text()),
        ];
        if let Some(sug) = &ed.suggestion {
            spans.push(Span::styled(sug.clone(), theme::faint()));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{23ce} confirm   \u{b7}   \u{2192} complete   \u{b7}   esc cancel",
            theme::faint(),
        )));
    } else {
        for (i, root) in ed.roots.iter().enumerate() {
            let selected = i == ed.cursor;
            let style = if selected {
                theme::selection().patch(theme::text())
            } else {
                theme::text()
            };
            let prefix = if selected { "\u{25b8} " } else { "  " };
            lines.push(Line::from(Span::styled(format!("{prefix}{root}"), style)));
        }
        // "+ add root…" sentinel row
        let add_selected = ed.cursor == ed.roots.len();
        let add_style = if add_selected {
            theme::selection().patch(theme::accent_soft())
        } else {
            theme::accent_soft()
        };
        let prefix = if add_selected { "\u{25b8} " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{ADD_ROOT}"),
            add_style,
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{2191}\u{2193} move   \u{b7}   \u{23ce} edit   \u{b7}   a add   \u{b7}   d remove   \u{b7}   esc done",
            theme::faint(),
        )));
    }

    // 2 header lines (title + blank) precede the root rows; in edit mode the
    // list is replaced by the text input so the cursor line is irrelevant.
    let cursor_line = if ed.input.is_some() { 0 } else { 2 + ed.cursor };
    crate::tui::widgets::render_scrolling_lines(f, area, lines, cursor_line);
}

/// Left-elide `s` to at most `max` columns: if longer, keep the tail and
/// prefix `…` (so the most-specific path component stays visible).
fn elide_left(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(len - (max - 1)).collect();
    format!("\u{2026}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_suggestion_completes_first_matching_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("aardvark")).unwrap();
        std::fs::create_dir(dir.path().join("abacus")).unwrap();
        std::fs::create_dir(dir.path().join("zebra")).unwrap();
        let base = dir.path().display().to_string();

        // leaf "aa" → first match "aardvark" → suffix "rdvark/"
        assert_eq!(
            dir_suggestion(&format!("{base}/aa")),
            Some("rdvark/".to_string())
        );
        // leaf "" (trailing slash) → first child alphabetically ("aardvark") → "aardvark/"
        assert_eq!(
            dir_suggestion(&format!("{base}/")),
            Some("aardvark/".to_string())
        );
        // no match → None
        assert_eq!(dir_suggestion(&format!("{base}/q")), None);
        // no slash / unreadable parent → None (never errors)
        assert_eq!(dir_suggestion("relativeish"), None);
        assert_eq!(dir_suggestion("/no/such/parent/x"), None);
    }

    #[test]
    fn membership_labels_on_demand_keys() {
        let mut working = Profiles::default();
        working.on_demand.push("pixijs@x".to_string());
        working.universal.push("serena@x".to_string());

        // On-demand key → labelled, not rendered as untriaged "—".
        assert_eq!(
            membership(&working, "pixijs@x"),
            vec!["On-demand".to_string()]
        );
        // Universal still short-circuits first.
        assert_eq!(
            membership(&working, "serena@x"),
            vec!["Universal".to_string()]
        );
        // Genuinely unassigned key → still empty.
        assert!(membership(&working, "other@x").is_empty());
    }
}
