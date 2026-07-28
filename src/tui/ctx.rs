use std::path::PathBuf;

use crate::account::paths::ClaudePaths;
use crate::account::store::Store;

/// Handles every view needs, resolved once at startup. Holds no UI state.
pub struct AppCtx {
    pub store: Store,
    pub claude: ClaudePaths,
    /// `$HOME`. Consumed by Plan 02 (account switch passes it to `swap::switch`);
    /// gathered now so the shared context is stable across plans.
    pub home: PathBuf,
    /// The cc-loadout data root. Consumed by Plan 03 (schedule writes under it).
    pub data_root: PathBuf,
    /// Path to profiles.json (resolved via `profile::config::profiles_path`).
    pub cfg_path: PathBuf,
    /// `installed_plugins.json` path (for the Profile wizard's inventory).
    pub registry_path: PathBuf,
    /// Working directory the hub was launched from (for the profile section).
    pub cwd: PathBuf,
}
