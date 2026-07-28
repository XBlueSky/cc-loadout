use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

use crate::util::atomicfile;

/// Read the full `credentials.json` object (the `{ "claudeAiOauth": {...} }` wrapper).
pub fn read_credentials(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

/// Write a JSON value to `path` atomically with mode 0600. Used for both the live
/// `credentials.json` and the per-account snapshots (`credentials.json`/`oauth.json`).
pub fn write_credentials(path: &Path, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomicfile::write_atomic(path, &bytes, 0o600)
}

/// Read the `oauthAccount` block from the main config, if present.
pub fn read_oauth_account(config_path: &Path) -> Result<Option<Value>> {
    if !config_path.exists() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(config_path).with_context(|| format!("reading {}", config_path.display()))?;
    let cfg: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", config_path.display()))?;
    Ok(cfg.get("oauthAccount").cloned())
}

/// Replace ONLY the `oauthAccount` key in the main config, preserving every other
/// key and its order (`serde_json` `preserve_order`). Writes atomically, mode 0600.
pub fn write_oauth_account(config_path: &Path, oauth_account: &Value) -> Result<()> {
    crate::util::jsonmerge::merge_object(config_path, 0o600, |map| {
        map.insert("oauthAccount".to_string(), oauth_account.clone());
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn credentials_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".credentials.json");
        let v = json!({"claudeAiOauth": {"accessToken": "x", "refreshToken": "y"}});
        write_credentials(&p, &v).unwrap();
        let back = read_credentials(&p).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn read_oauth_account_absent_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(&p, br#"{"numStartups": 3}"#).unwrap();
        assert!(read_oauth_account(&p).unwrap().is_none());
    }

    #[test]
    fn surgical_write_preserves_order_and_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claude.json");
        std::fs::write(
            &p,
            br#"{"alpha": 1, "oauthAccount": {"emailAddress": "old@x"}, "zeta": 2}"#,
        )
        .unwrap();

        let new_oauth = json!({"emailAddress": "new@x", "organizationUuid": "o1"});
        write_oauth_account(&p, &new_oauth).unwrap();

        let raw = std::fs::read_to_string(&p).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(parsed["oauthAccount"]["emailAddress"], json!("new@x"));
        assert_eq!(parsed["alpha"], json!(1));
        assert_eq!(parsed["zeta"], json!(2));

        let keys: Vec<&str> = parsed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["alpha", "oauthAccount", "zeta"]);
    }

    #[test]
    fn surgical_write_creates_file_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".claude.json");
        write_oauth_account(&p, &json!({"emailAddress": "a@x"})).unwrap();
        let back = read_oauth_account(&p).unwrap().unwrap();
        assert_eq!(back["emailAddress"], json!("a@x"));
    }
}
