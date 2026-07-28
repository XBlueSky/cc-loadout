use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState};
use ratatui::Frame;

use crate::tui::theme;

/// A checkbox list: navigate with ↑↓/j/k, toggle with Space. Enter is NOT
/// handled here (the owning view advances the wizard).
// Used by Assign/Plugins sub-views (Tasks 6-8).
#[allow(dead_code)]
pub struct MultiSelect {
    items: Vec<String>,
    checked: Vec<bool>,
    cursor: usize,
    annotate: Vec<String>,
}

#[allow(dead_code)]
impl MultiSelect {
    pub fn new(items: Vec<String>, preselected: &[String]) -> Self {
        let checked = items.iter().map(|i| preselected.contains(i)).collect();
        MultiSelect {
            items,
            checked,
            cursor: 0,
            annotate: Vec::new(),
        }
    }

    /// Mark `keys` with a dim `(now global)` suffix when rendered.
    pub fn annotated(mut self, keys: &[String]) -> Self {
        self.annotate = keys.to_vec();
        self
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.items.is_empty() {
            return;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1) % self.items.len();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = (self.cursor + self.items.len() - 1) % self.items.len();
            }
            KeyCode::Char(' ') => {
                self.checked[self.cursor] = !self.checked[self.cursor];
            }
            _ => {}
        }
    }

    pub fn selected(&self) -> Vec<String> {
        self.items
            .iter()
            .zip(&self.checked)
            .filter(|(_, &c)| c)
            .map(|(i, _)| i.clone())
            .collect()
    }

    pub fn render(&self, f: &mut Frame, area: Rect, title: &str) {
        // Title label line rendered as first (non-selectable) item is not possible in
        // ratatui List, so we rely on the owning view to reserve space or the tab title.
        // Instead, prepend a dim title row as a non-interactive ListItem.
        let mut rows: Vec<ListItem> = Vec::with_capacity(self.items.len() + 2);
        rows.push(ListItem::new(Line::from(Span::styled(
            title.to_string(),
            theme::dim(),
        ))));
        rows.push(ListItem::new(Line::raw("")));
        for (item, &c) in self.items.iter().zip(&self.checked) {
            let (mark_str, mark_style) = if c {
                ("[x]", theme::accent())
            } else {
                ("[ ]", theme::faint())
            };
            let mut line = vec![
                Span::styled(mark_str, mark_style),
                Span::styled(format!(" {item}"), theme::text()),
            ];
            if self.annotate.contains(item) {
                line.push(Span::styled("  (now global)", theme::faint()));
            }
            rows.push(ListItem::new(Line::from(line)));
        }
        let mut state = ListState::default();
        if !self.items.is_empty() {
            // offset by 2 for the title + blank rows
            state.select(Some(self.cursor.min(self.items.len() - 1) + 2));
        }
        // No border block — render directly into area.
        let list = List::new(rows)
            .highlight_style(theme::selection())
            .highlight_symbol("▸ ")
            .highlight_spacing(HighlightSpacing::Always);
        f.render_stateful_widget(list, area, &mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::from(c)
    }

    #[test]
    fn preselects_and_toggles() {
        let mut ms = MultiSelect::new(vec!["a".into(), "b".into(), "c".into()], &["b".to_string()]);
        assert_eq!(ms.selected(), vec!["b".to_string()]);
        // cursor at 0 (a) -> toggle on
        ms.on_key(k(KeyCode::Char(' ')));
        assert_eq!(ms.selected(), vec!["a".to_string(), "b".to_string()]);
        // move to b, toggle off
        ms.on_key(k(KeyCode::Down));
        ms.on_key(k(KeyCode::Char(' ')));
        assert_eq!(ms.selected(), vec!["a".to_string()]);
    }

    #[test]
    fn nav_wraps_and_empty_is_safe() {
        let mut ms = MultiSelect::new(vec!["x".into()], &[]);
        ms.on_key(k(KeyCode::Up)); // wrap, stays on x
        ms.on_key(k(KeyCode::Char(' ')));
        assert_eq!(ms.selected(), vec!["x".to_string()]);

        let mut empty = MultiSelect::new(vec![], &[]);
        empty.on_key(k(KeyCode::Char(' '))); // no panic
        assert!(empty.selected().is_empty());
    }

    #[test]
    fn annotated_items_render_now_global_suffix() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let ms =
            MultiSelect::new(vec!["a@m".into(), "b@m".into()], &[]).annotated(&["a@m".to_string()]);
        let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
        t.draw(|f| ms.render(f, f.area(), "pick")).unwrap();
        let text: String = t
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("a@m"));
        assert!(
            text.contains("now global"),
            "annotated item shows suffix: {text}"
        );
    }
}
