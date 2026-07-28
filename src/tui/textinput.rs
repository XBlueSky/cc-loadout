use ratatui::crossterm::event::{KeyCode, KeyEvent};

/// A minimal single-line text input: a `String` plus a byte-unaware char cursor.
/// Handles printable chars, Backspace, Delete, Left/Right, Home/End. Enter/Esc
/// are NOT consumed (the owning view decides commit/cancel).
// Used by future sub-view text-entry steps (Tasks 6-8).
#[allow(dead_code)]
pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
}

#[allow(dead_code)]
impl TextInput {
    pub fn new(initial: &str) -> Self {
        let chars: Vec<char> = initial.chars().collect();
        let cursor = chars.len();
        TextInput { chars, cursor }
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    /// True when the cursor is at the end of the buffer (used to decide whether
    /// `→` should accept a trailing ghost suggestion vs. move the cursor).
    pub fn at_end(&self) -> bool {
        self.cursor == self.chars.len()
    }

    /// Returns true if the key was consumed (caller should not act on it further).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                }
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                if self.cursor < self.chars.len() {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.chars.len();
                true
            }
            // Enter / Esc / others: not consumed — the view handles commit/cancel.
            _ => false,
        }
    }

    /// The value with a `▏` caret inserted at the cursor, for display.
    pub fn render_line(&self) -> String {
        let mut s: String = self.chars[..self.cursor].iter().collect();
        s.push('\u{258f}'); // ▏
        s.extend(&self.chars[self.cursor..]);
        s
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
    fn types_and_backspaces() {
        let mut ti = TextInput::new("");
        for c in "06:00".chars() {
            assert!(ti.handle_key(k(KeyCode::Char(c))));
        }
        assert_eq!(ti.value(), "06:00");
        assert!(ti.handle_key(k(KeyCode::Backspace)));
        assert_eq!(ti.value(), "06:0");
    }

    #[test]
    fn cursor_left_insert_middle() {
        let mut ti = TextInput::new("0600");
        ti.handle_key(k(KeyCode::Left)); // between 0 and 0 -> "060|0"
        ti.handle_key(k(KeyCode::Left)); // "06|00"
        ti.handle_key(k(KeyCode::Char(':')));
        assert_eq!(ti.value(), "06:00");
    }

    #[test]
    fn enter_and_esc_not_consumed() {
        let mut ti = TextInput::new("x");
        assert!(!ti.handle_key(k(KeyCode::Enter)));
        assert!(!ti.handle_key(k(KeyCode::Esc)));
    }

    #[test]
    fn home_end_and_delete() {
        let mut ti = TextInput::new("ab");
        ti.handle_key(k(KeyCode::Home));
        ti.handle_key(k(KeyCode::Delete)); // removes 'a'
        assert_eq!(ti.value(), "b");
        ti.handle_key(k(KeyCode::End));
        ti.handle_key(k(KeyCode::Char('c')));
        assert_eq!(ti.value(), "bc");
    }

    #[test]
    fn at_end_tracks_cursor() {
        let mut ti = TextInput::new("ab");
        assert!(ti.at_end(), "new() parks the cursor at the end");
        ti.handle_key(k(KeyCode::Left));
        assert!(!ti.at_end(), "cursor moved off the end");
        ti.handle_key(k(KeyCode::End));
        assert!(ti.at_end(), "End returns to the end");
    }
}
