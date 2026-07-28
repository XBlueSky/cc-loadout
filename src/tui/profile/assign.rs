use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::Frame;

use crate::profile::config::Profiles;
use crate::tui::theme;

/// The label for the "create a new profile" target option.
/// Used both in the target list and in the key handler; kept in one place so
/// a typo cannot silently desync them.
const NEW_PROFILE: &str = "+ New profile\u{2026}";
const ON_DEMAND: &str = "On-demand";

/// State for the Assign sub-view — walks unassigned plugins one at a time.
pub struct AssignState {
    /// Plugin keys to triage, in order.
    pub queue: Vec<String>,
    /// Index of the current plugin being placed.
    pub idx: usize,
    /// Selected target option cursor.
    pub cursor: usize,
    /// Some while the user is typing a new profile name.
    pub naming: Option<crate::tui::textinput::TextInput>,
}

impl AssignState {
    pub fn new(queue: Vec<String>) -> Self {
        AssignState {
            queue,
            idx: 0,
            cursor: 0,
            naming: None,
        }
    }

    /// Compute target options for the current plugin.
    /// ["Universal"] ++ working.profiles.keys() ++ ["On-demand", "+ New profile…", "Leave unassigned"]
    pub fn target_options(&self, working: &Profiles) -> Vec<String> {
        let mut opts = vec!["Universal".to_string()];
        opts.extend(working.profiles.keys().cloned());
        opts.push(ON_DEMAND.to_string());
        opts.push(NEW_PROFILE.to_string());
        opts.push("Leave unassigned".to_string());
        opts
    }

    /// Returns true if the queue is exhausted.
    pub fn is_done(&self) -> bool {
        self.idx >= self.queue.len()
    }

    /// Current plugin key being triaged.
    pub fn current_plugin(&self) -> Option<&str> {
        self.queue.get(self.idx).map(|s| s.as_str())
    }

    /// Handle a key event. Returns `true` when the assign flow is complete (go
    /// back to Board). `working` is mutated on successful placement.
    pub fn handle_key(&mut self, key: KeyEvent, working: &mut Profiles) -> bool {
        if self.naming.is_some() {
            // Naming mode: feed key to TextInput; Enter commits, Esc cancels.
            match key.code {
                KeyCode::Enter => {
                    let name = self.naming.as_ref().unwrap().value();
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        // ignore empty — stay in naming
                        return false;
                    }
                    // Insert profile if not already present.
                    working.profiles.entry(name.clone()).or_default();
                    // Push the current plugin into that profile (dedup).
                    if let Some(plugin) = self.queue.get(self.idx).cloned() {
                        let p = working.profiles.get_mut(&name).unwrap();
                        if !p.plugins.contains(&plugin) {
                            p.plugins.push(plugin);
                        }
                    }
                    self.naming = None;
                    self.advance();
                    return self.is_done();
                }
                KeyCode::Esc => {
                    self.naming = None;
                    return false;
                }
                _ => {
                    self.naming.as_mut().unwrap().handle_key(key);
                    return false;
                }
            }
        }

        // Normal mode.
        let opts = self.target_options(working);
        let n = opts.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = (self.cursor + n - 1) % n;
                false
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1) % n;
                false
            }
            KeyCode::Enter => {
                let selected = opts[self.cursor].clone();
                if selected == "Universal" {
                    if let Some(plugin) = self.queue.get(self.idx).cloned() {
                        if !working.universal.contains(&plugin) {
                            working.universal.push(plugin);
                        }
                    }
                    self.advance();
                    self.is_done()
                } else if selected == ON_DEMAND {
                    if let Some(plugin) = self.queue.get(self.idx).cloned() {
                        if !working.on_demand.contains(&plugin) {
                            working.on_demand.push(plugin);
                        }
                    }
                    self.advance();
                    self.is_done()
                } else if selected == NEW_PROFILE {
                    self.naming = Some(crate::tui::textinput::TextInput::new(""));
                    false
                } else if selected == "Leave unassigned" {
                    self.advance();
                    self.is_done()
                } else {
                    // A named profile.
                    if let Some(plugin) = self.queue.get(self.idx).cloned() {
                        if let Some(p) = working.profiles.get_mut(&selected) {
                            if !p.plugins.contains(&plugin) {
                                p.plugins.push(plugin);
                            }
                        }
                    }
                    self.advance();
                    self.is_done()
                }
            }
            KeyCode::Esc => {
                // Abandon — signal done so the parent returns to Board.
                true
            }
            _ => false,
        }
    }

    fn advance(&mut self) {
        self.idx += 1;
        self.cursor = 0;
    }
}

/// Render the Assign sub-view.
pub fn render(state: &AssignState, working: &Profiles, f: &mut Frame, area: Rect) {
    let left = state.queue.len().saturating_sub(state.idx);
    let current = state.current_plugin().unwrap_or("(done)");

    let opts = state.target_options(working);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut cursor_line = 0usize;

    // Header.
    lines.push(Line::from(vec![
        Span::styled("ASSIGN PLUGINS    ", theme::accent()),
        Span::styled(format!("{left} left"), theme::dim()),
    ]));
    lines.push(Line::from(""));

    // Current plugin.
    lines.push(Line::from(vec![
        Span::styled("plugin  ", theme::dim()),
        Span::styled(current.to_string(), theme::text()),
    ]));
    lines.push(Line::from(""));

    // Target options or naming prompt.
    if let Some(ti) = &state.naming {
        // Show the new-profile name prompt (like the wizard's Name step).
        lines.push(Line::from(vec![
            Span::styled("name    ", theme::dim()),
            Span::styled(ti.render_line(), theme::text()),
        ]));
    } else {
        for (i, opt) in opts.iter().enumerate() {
            let style = if i == state.cursor {
                theme::selection().patch(theme::text())
            } else {
                theme::text()
            };
            let prefix = if i == state.cursor { "\u{25b8} " } else { "  " };
            if i == state.cursor {
                cursor_line = lines.len();
            }
            lines.push(Line::from(vec![Span::styled(
                format!("{prefix}{opt}"),
                style,
            )]));
        }
    }

    crate::tui::widgets::render_scrolling_lines(f, area, lines, cursor_line);
}
