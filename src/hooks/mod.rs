//! Hook entry points invoked by the bundled plugin's shims.
//!
//! Claude Code writes a JSON payload to the hook's stdin. The shims pass it
//! through untouched; everything below is the real work. Nothing here prints to
//! stdout on success — a SessionStart hook's stdout is injected into the
//! session's context.

pub mod legacy;

use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The fields cc-loadout needs from the hook payload.
#[derive(Debug, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
}

/// Parse the payload. Malformed input yields defaults rather than an error: a
/// hook must degrade silently, never block a session from starting.
pub fn parse_input(raw: &str) -> HookInput {
    serde_json::from_str(raw).unwrap_or_default()
}

/// `<config_dir>/settings.json`, honouring `$CLAUDE_CONFIG_DIR` the same way
/// `profile::discover::resolve_registry_path` does.
pub fn settings_path(home: &Path, config_override: Option<&Path>) -> PathBuf {
    let base = match config_override {
        Some(p) => p.to_path_buf(),
        None => home.join(".claude"),
    };
    base.join("settings.json")
}

/// SessionStart: re-assert plugin scope, and finish the one-time migration off
/// the retired `settings.json` hooks.
///
/// It no longer publishes the session id through `$CLAUDE_ENV_FILE`: Claude
/// Code exports `CLAUDE_CODE_SESSION_ID` to every Bash tool subprocess itself
/// (and keeps it current across `/clear` and `/resume`), whereas the env-file
/// append re-ran on every compact/resume and piled duplicate lines onto every
/// Bash command of a long session.
///
/// Every step below is deliberately best-effort (`if let Ok`/`let _ =`): a
/// SessionStart hook must never block a session from starting. If a future
/// edit adds another fallible step, it must be swallowed the same way — do
/// not propagate it just because the function is typed `-> Result<()>` and
/// called with `?` at the dispatch site; today nothing inside can actually
/// return `Err`, and that `?` is inert on purpose.
pub fn session_start(home: &Path, config_override: Option<&Path>) -> Result<()> {
    let cfg_path = crate::profile::config::profiles_path(home);
    if let Ok(cfg) = crate::profile::config::load(&cfg_path) {
        let registry = crate::profile::discover::resolve_registry_path(home, config_override);
        let _ = crate::profile::registry::promote_all(&cfg, &registry);
    }

    let _ = legacy::remove_legacy_hooks(&settings_path(home, config_override));
    Ok(())
}

/// SessionEnd: drop every on-demand hold this session took out.
///
/// Same rule as `session_start`: this must never block a session from
/// ending, so the one fallible step is swallowed (`let _ =`) rather than
/// propagated. A future edit that adds another step must follow suit.
pub fn session_end(raw: &str) -> Result<()> {
    let input = parse_input(raw);
    if input.session_id.is_empty() || input.cwd.is_empty() {
        return Ok(());
    }
    let _ = crate::profile::on_demand::release_all(Path::new(&input.cwd), &input.session_id);
    Ok(())
}
