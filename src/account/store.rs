use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::util::atomicfile;

/// Persisted metadata for one saved account.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountMeta {
    pub email: String,
    #[serde(default)]
    pub org_uuid: Option<String>,
    #[serde(default)]
    pub org_name: Option<String>,
    /// Unix epoch seconds.
    pub added_at: i64,
    /// Unix epoch seconds.
    #[serde(default)]
    pub last_used: Option<i64>,
    /// Unix epoch seconds of the last successful `account prime`. `None` if never.
    #[serde(default)]
    pub last_primed: Option<i64>,
}

/// Schema version of `state.json`. Bump when the on-disk shape changes
/// incompatibly. A file whose version is NEWER than this is rejected (fail-fast)
/// rather than rewritten by an older binary that does not understand it.
pub const STATE_VERSION: u32 = 1;

fn default_state_version() -> u32 {
    STATE_VERSION
}

/// The whole switcher state file (`state.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "default_state_version")]
    pub version: u32,
    #[serde(default)]
    pub active_alias: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, AccountMeta>,
}

impl Default for State {
    fn default() -> Self {
        State {
            version: STATE_VERSION,
            active_alias: None,
            accounts: BTreeMap::new(),
        }
    }
}

/// On-disk layout under the data root (`~/.local/share/cc-loadout`).
#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(data_root: &Path) -> Self {
        Store {
            root: data_root.to_path_buf(),
        }
    }

    pub fn account_dir(&self, alias: &str) -> PathBuf {
        self.root.join("accounts").join(alias)
    }
    pub fn credentials_snapshot(&self, alias: &str) -> PathBuf {
        self.account_dir(alias).join("credentials.json")
    }
    pub fn oauth_snapshot(&self, alias: &str) -> PathBuf {
        self.account_dir(alias).join("oauth.json")
    }
    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(".lock")
    }

    pub fn data_root(&self) -> &std::path::Path {
        &self.root
    }

    /// Create the account dir (and parents) and tighten the data dirs to 0700,
    /// so credential snapshots are not exposed via world-traversable directories.
    pub fn ensure_account_dir(&self, alias: &str) -> Result<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let dir = self.account_dir(alias);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let accounts = self.root.join("accounts");
        for d in [self.root.as_path(), accounts.as_path(), dir.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 700 {}", d.display()))?;
        }
        Ok(dir)
    }

    pub fn load_state(&self) -> Result<State> {
        let p = self.state_path();
        if !p.exists() {
            return Ok(State::default());
        }
        let bytes = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
        let state: State =
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", p.display()))?;
        if state.version > STATE_VERSION {
            bail!(
                "{} has schema version {} which is newer than this cc-loadout understands (max {}); \
                 upgrade cc-loadout — refusing to touch live config with an older binary",
                p.display(),
                state.version,
                STATE_VERSION
            );
        }
        Ok(state)
    }

    pub fn save_state(&self, state: &State) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state)?;
        atomicfile::write_atomic(&self.state_path(), &bytes, 0o600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_state_loads_default() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let state = store.load_state().unwrap();
        assert!(state.active_alias.is_none());
        assert!(state.accounts.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let mut state = State {
            active_alias: Some("work".into()),
            ..Default::default()
        };
        state.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b.com".into(),
                org_uuid: Some("o1".into()),
                org_name: None,
                added_at: 1700000000,
                last_used: None,
                last_primed: None,
            },
        );
        store.save_state(&state).unwrap();

        let loaded = store.load_state().unwrap();
        assert_eq!(loaded.active_alias.as_deref(), Some("work"));
        assert_eq!(loaded.accounts["work"].email, "a@b.com");
        assert_eq!(loaded.accounts["work"].added_at, 1700000000);
    }

    #[test]
    fn path_helpers() {
        let store = Store::new(std::path::Path::new("/data"));
        assert_eq!(
            store.credentials_snapshot("work"),
            std::path::Path::new("/data/accounts/work/credentials.json")
        );
        assert_eq!(
            store.oauth_snapshot("work"),
            std::path::Path::new("/data/accounts/work/oauth.json")
        );
        assert_eq!(store.state_path(), std::path::Path::new("/data/state.json"));
        assert_eq!(store.lock_path(), std::path::Path::new("/data/.lock"));
    }

    #[test]
    fn default_state_carries_current_version() {
        assert_eq!(State::default().version, STATE_VERSION);
    }

    #[test]
    fn unversioned_state_loads_as_v1() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        // Old file with no `version` key at all.
        std::fs::write(
            store.state_path(),
            br#"{"active_alias":"work","accounts":{}}"#,
        )
        .unwrap();
        let s = store.load_state().unwrap();
        assert_eq!(s.version, 1);
        assert_eq!(s.active_alias.as_deref(), Some("work"));
    }

    #[test]
    fn newer_version_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        std::fs::write(store.state_path(), br#"{"version":999,"accounts":{}}"#).unwrap();
        let err = store.load_state().unwrap_err();
        assert!(err.to_string().contains("newer"));
    }
}
