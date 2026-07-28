use anyhow::{bail, Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::account::paths;
use crate::account::store::Store;
use crate::account::{creds, validate_alias};

/// Ensure `<data_root>/run/<alias>/cfg` exists (0700), seeded with the account's
/// snapshot credentials/oauth and a `plugins` symlink to the live install so
/// `enabledPlugins` resolves. Returns the dir to pass as CLAUDE_CONFIG_DIR.
///
/// `live_plugins` is the real `~/.claude/plugins` directory.
pub fn ensure_isolated_dir(
    store: &Store,
    alias: &str,
    live_plugins: &Path,
    home: &Path,
) -> Result<PathBuf> {
    validate_alias(alias)?;
    let snap_creds = store.credentials_snapshot(alias);
    if !snap_creds.exists() {
        bail!(
            "no credential snapshot for '{alias}' at {}",
            snap_creds.display()
        );
    }

    let cfg_dir = store.account_dir(alias).join("run").join("cfg");
    std::fs::create_dir_all(&cfg_dir).with_context(|| format!("creating {}", cfg_dir.display()))?;
    if let Some(parent) = cfg_dir.parent() {
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    std::fs::set_permissions(&cfg_dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 700 {}", cfg_dir.display()))?;

    // Seed credentials + oauth identity (write both forms, as prime does).
    let eph = paths::resolve(home, Some(&cfg_dir));
    let creds_val = creds::read_credentials(&snap_creds)?;
    creds::write_credentials(&eph.credentials, &creds_val)?;
    if let Ok(oauth) = creds::read_credentials(&store.oauth_snapshot(alias)) {
        creds::write_oauth_account(&eph.main_config, &oauth)?;
        creds::write_oauth_account(&cfg_dir.join(".claude.json"), &oauth)?;
    }

    // Symlink the installed plugins so the isolated run sees the same install.
    let link = cfg_dir.join("plugins");
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&link)
                .with_context(|| format!("removing stale plugins symlink at {}", link.display()))?;
        } else if meta.is_dir() {
            std::fs::remove_dir_all(&link)
                .with_context(|| format!("removing stale plugins dir at {}", link.display()))?;
        }
    }
    if live_plugins.exists() {
        std::os::unix::fs::symlink(live_plugins, &link)
            .with_context(|| format!("symlinking plugins into {}", cfg_dir.display()))?;
    }

    Ok(cfg_dir)
}

#[cfg(test)]
mod tests {
    use crate::account::{creds, store::Store};
    use serde_json::json;

    use super::*;

    #[test]
    fn seeds_credentials_and_symlinks_plugins() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        store.ensure_account_dir("work").unwrap();
        creds::write_credentials(
            &store.credentials_snapshot("work"),
            &json!({"claudeAiOauth": {"accessToken": "A", "refreshToken": "R"}}),
        )
        .unwrap();

        // a fake live plugins dir with one marker file
        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();
        std::fs::write(live_plugins.join("installed_plugins.json"), b"{}").unwrap();

        let cfg = ensure_isolated_dir(&store, "work", &live_plugins, home.path()).unwrap();

        // credentials present in the isolated dir
        let c = creds::read_credentials(&cfg.join(".credentials.json")).unwrap();
        assert_eq!(c["claudeAiOauth"]["accessToken"], json!("A"));
        // plugins reachable through the symlink
        assert!(cfg.join("plugins").join("installed_plugins.json").exists());
    }

    #[test]
    fn missing_snapshot_errors() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let lp = home.path().join("plugins");
        std::fs::create_dir_all(&lp).unwrap();
        assert!(ensure_isolated_dir(&store, "ghost", &lp, home.path())
            .unwrap_err()
            .to_string()
            .contains("snapshot"));
    }

    #[test]
    fn repeat_run_reuses_dir_idempotently() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        store.ensure_account_dir("work").unwrap();
        creds::write_credentials(
            &store.credentials_snapshot("work"),
            &json!({"claudeAiOauth": {"accessToken": "A", "refreshToken": "R"}}),
        )
        .unwrap();
        let live_plugins = home.path().join(".claude").join("plugins");
        std::fs::create_dir_all(&live_plugins).unwrap();
        std::fs::write(live_plugins.join("installed_plugins.json"), b"{}").unwrap();

        let first = ensure_isolated_dir(&store, "work", &live_plugins, home.path()).unwrap();
        let second = ensure_isolated_dir(&store, "work", &live_plugins, home.path()).unwrap();
        assert_eq!(first, second);
        assert!(second
            .join("plugins")
            .join("installed_plugins.json")
            .exists());
    }
}
