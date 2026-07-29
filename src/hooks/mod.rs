//! Hook entry points invoked by the bundled plugin's shims.
//!
//! Claude Code writes a JSON payload to the hook's stdin. The shims pass it
//! through untouched; everything below is the real work. Nothing here prints to
//! stdout on success — a SessionStart hook's stdout is injected into the
//! session's context.

pub mod legacy;

use anyhow::Result;
use serde::Deserialize;
use std::io::Write;
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

/// True only for a session id safe to interpolate, unquoted, into a shell
/// script that `$CLAUDE_ENV_FILE` will later be `source`d as. Hook stdin is
/// untrusted everywhere else in this module (see `parse_input`'s
/// `unwrap_or_default`), and this is the single most dangerous thing this
/// module does with that input: a JSON string can legally contain a newline
/// (which would split into a second shell command) or `$(...)`/backticks
/// (command substitution). Claude Code supplies a UUID today, so this is
/// latent rather than exploitable — but the trust boundary should be
/// consistent regardless of what the current producer happens to send.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// SessionStart: publish the session id, re-assert plugin scope, and finish the
/// one-time migration off the retired `settings.json` hooks.
///
/// Every step below is deliberately best-effort (`if let Ok`/`let _ =`): a
/// SessionStart hook must never block a session from starting. If a future
/// edit adds another fallible step, it must be swallowed the same way — do
/// not propagate it just because the function is typed `-> Result<()>` and
/// called with `?` at the dispatch site; today nothing inside can actually
/// return `Err`, and that `?` is inert on purpose.
pub fn session_start(home: &Path, config_override: Option<&Path>, raw: &str) -> Result<()> {
    let input = parse_input(raw);

    // `profile on-demand acquire` needs the id, and a hook can only publish an
    // env var to the rest of the session through $CLAUDE_ENV_FILE. Anything
    // that fails the safety check is skipped entirely rather than escaped or
    // truncated: writing a sanitized guess would still corrupt trust in a
    // file the shell is about to execute, whereas skipping just means
    // `profile on-demand acquire` won't find a session id — which it already
    // handles by erroring with a clear message.
    if is_safe_session_id(&input.session_id) {
        if let Some(env_file) = std::env::var_os("CLAUDE_ENV_FILE") {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(env_file)
            {
                let _ = writeln!(f, "export CC_LOADOUT_SESSION_ID={}", input.session_id);
            }
        }
    }

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
