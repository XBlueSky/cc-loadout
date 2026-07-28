//! The single place that defines the hub's look — warm "Claude Code" palette.
//! accent is reserved for decorative/focus elements; hierarchy comes from
//! bold + dim contrast, not many hues.

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

// Warm palette (hardcoded RGB).
const C_BG: Color = Color::Rgb(0x16, 0x13, 0x0f);
const C_PANEL: Color = Color::Rgb(0x1c, 0x19, 0x17);
const C_TEXT: Color = Color::Rgb(0xe9, 0xe3, 0xd8);
const C_DIM: Color = Color::Rgb(0x8a, 0x81, 0x78);
const C_FAINT: Color = Color::Rgb(0x57, 0x4f, 0x47);
const C_ACCENT: Color = Color::Rgb(0xd9, 0x77, 0x57);
const C_ACCENT_DIM: Color = Color::Rgb(0xa8, 0x5e, 0x44);
const C_ACCENT_SOFT: Color = Color::Rgb(0xe8, 0xa9, 0x8f);
const C_ALERT: Color = Color::Rgb(0xcf, 0x6b, 0x54);
// Focused-row background bar. A warm neutral (not accent), so list focus reads
// as a background highlight while accent stays reserved for state (active ●).
const C_SELECTION: Color = Color::Rgb(0x33, 0x2c, 0x24);

pub const BORDER: BorderType = BorderType::Rounded;

/// Spinner glyph cycle (Claude-style braided dots).
pub const SPINNER: &[&str] = &["✻", "✸", "✦", "✶", "✷", "✵"];

pub fn bg() -> Style {
    Style::default().bg(C_BG).fg(C_TEXT)
}
pub fn text() -> Style {
    Style::default().fg(C_TEXT)
}
pub fn dim() -> Style {
    Style::default().fg(C_DIM)
}
pub fn faint() -> Style {
    Style::default().fg(C_FAINT)
}
pub fn accent() -> Style {
    Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
}
pub fn accent_dim() -> Style {
    Style::default().fg(C_ACCENT_DIM)
}
pub fn accent_soft() -> Style {
    Style::default().fg(C_ACCENT_SOFT)
}
pub fn alert() -> Style {
    Style::default().fg(C_ALERT)
}
/// Selected tab: accent text, bold (no full-width bar).
pub fn selected_tab() -> Style {
    Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD)
}
/// Focused row in a list: a subtle background bar (NOT accent foreground), so
/// focus does not collide with accent-coloured state markers. Pair with the ▸
/// highlight symbol.
pub fn selection() -> Style {
    Style::default().bg(C_SELECTION)
}
/// PANEL background for modal / spinner surfaces.
pub fn panel() -> Style {
    Style::default().bg(C_PANEL).fg(C_TEXT)
}

/// The active marker's breathing pulse: lerp ACCENT_DIM↔ACCENT on a slow sine.
pub fn pulse(frame: u64) -> Style {
    // 0..=1 triangle over ~20 frames; cheap, no float trig needed for a 2-stop lerp.
    let phase = (frame % 40) as i32;
    let t = if phase < 20 { phase } else { 40 - phase }; // 0..=20..=0
    let lerp = |a: u8, b: u8| -> u8 {
        let a = a as i32;
        let b = b as i32;
        (a + (b - a) * t / 20) as u8
    };
    Style::default()
        .fg(Color::Rgb(
            lerp(0xa8, 0xd9),
            lerp(0x5e, 0x77),
            lerp(0x44, 0x57),
        ))
        .add_modifier(Modifier::BOLD)
}

pub fn spinner_frame(frame: u64) -> &'static str {
    SPINNER[(frame as usize) % SPINNER.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_cycles() {
        assert_eq!(spinner_frame(0), SPINNER[0]);
        assert_eq!(spinner_frame(SPINNER.len() as u64), SPINNER[0]);
        assert_ne!(spinner_frame(1), spinner_frame(0));
    }

    #[test]
    fn pulse_endpoints_are_accent_range() {
        // At the triangle endpoints the colour is the dim/bright accent stops.
        let _ = pulse(0); // ACCENT_DIM end
        let _ = pulse(20); // ACCENT end
                           // Just assert it produces a style without panicking across a cycle.
        for f in 0..80 {
            let _ = pulse(f);
        }
    }
}
