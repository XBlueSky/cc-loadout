//! Inline ratatui hub panel. Thin shell over the domain layer; launched only
//! for interactive (TTY) invocations. The hub renders as a fixed-height panel
//! *inline* in the terminal (no alternate screen), so the user's prompt and
//! scrollback stay visible above it.

pub mod accounts;
pub mod app;
pub mod ctx;
pub mod job;
pub mod multiselect;
pub mod overview;
pub mod profile;
pub mod schedule;
pub mod snapshot;
pub mod tasks;
pub mod textinput;
pub mod theme;
pub mod view;
pub mod widgets;

/// Tab index constants for deep-linking entry points into the hub.
pub const TAB_OVERVIEW: usize = 0;
pub const TAB_ACCOUNTS: usize = 1;
pub const TAB_SCHEDULE: usize = 2;
pub const TAB_PROFILE: usize = 3;
pub const TAB_TASKS: usize = 4;

/// Whether to launch the inline hub panel. Today this is just "is stdout a TTY",
/// but it is a named seam so the rule is unit-testable and easy to extend.
pub fn should_launch_tui(is_tty: bool) -> bool {
    is_tty
}

/// Height (in terminal rows) of the inline hub panel, including the outer frame.
/// Sized to fit the tallest view's chrome + body with breathing room; capped to
/// the terminal height at launch.
const PANEL_ROWS: u16 = 26;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::{TerminalOptions, Viewport};
use time::OffsetDateTime;

use crate::tui::app::App;
use crate::tui::ctx::AppCtx;

/// `(now_ms_epoch, now_local)` for a single frame.
fn now_pair() -> (i64, OffsetDateTime) {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let now_local = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    (now_ms, now_local)
}

/// Launch the hub as an inline panel. `init_with_options(Viewport::Inline)`
/// enables raw mode and installs a panic hook that restores the terminal, but —
/// unlike `ratatui::init()` — it does NOT enter the alternate screen, so the
/// panel renders inline below the user's existing terminal output. On exit the
/// panel is wiped and `ratatui::restore()` disables raw mode.
///
/// The terminal is ALWAYS restored before any exec — this is the critical safety
/// property that prevents exec-ing while still in raw mode.
pub fn run(ctx: AppCtx, initial_tab: usize) -> Result<()> {
    let rows = ratatui::crossterm::terminal::size()
        .map(|(_, r)| r)
        .unwrap_or(24);
    let height = PANEL_ROWS.min(rows.max(1));
    let mut terminal = ratatui::init_with_options(TerminalOptions {
        viewport: Viewport::Inline(height),
    });
    let app_result = run_loop_capturing(&mut terminal, ctx, initial_tab);
    // Wipe the inline panel so the terminal returns to a clean prompt, leaving
    // the scrollback above the panel intact.
    clear_inline_panel(&mut terminal);
    ratatui::restore();
    match app_result {
        Ok(true) => crate::util::exec_claude_continue(), // terminal already restored
        Ok(false) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Clear the inline viewport region (and anything below it) without disturbing
/// the scrollback above: move the cursor to the panel's top-left and clear
/// downward. The panel's absolute start row comes from the viewport area.
fn clear_inline_panel(terminal: &mut ratatui::DefaultTerminal) {
    use ratatui::crossterm::{
        cursor::MoveTo,
        execute,
        terminal::{Clear, ClearType},
    };
    let area = terminal.get_frame().area();
    let _ = execute!(
        std::io::stdout(),
        MoveTo(area.x, area.y),
        Clear(ClearType::FromCursorDown)
    );
}

/// Inner event loop. Returns `Ok(true)` when the user requested a relaunch,
/// `Ok(false)` on a normal quit, and `Err` on I/O failure.
fn run_loop_capturing(
    terminal: &mut ratatui::DefaultTerminal,
    ctx: AppCtx,
    initial_tab: usize,
) -> Result<bool> {
    let mut app = App::new(ctx, initial_tab)?;
    while !app.should_quit && !app.relaunch {
        let (now_ms, now_local) = now_pair();
        terminal.draw(|f| app.render(f, now_ms, now_local))?;
        // Adaptive timeout: fast when animating (spinner, breathing, toast), slow
        // when idle (still ticks once per second so the clock + countdowns update).
        let timeout = if app.animating() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        };
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key)?;
                }
            }
        } else {
            app.tick(); // timeout elapsed: advance animation frame
        }
        app.drain_jobs(now_ms)?;
    }
    Ok(app.relaunch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches_only_on_a_tty() {
        assert!(should_launch_tui(true));
        assert!(!should_launch_tui(false));
    }
}
