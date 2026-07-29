//! Plugin-registry scope promotion.
//!
//! Claude Code's session-start loader can only resolve `enabledPlugins: true`
//! for a plugin that has a `scope: user` entry in `installed_plugins.json`. A
//! plugin auto-update can revert an entry to `scope: local` (bound to the repo
//! it was installed in), which makes the plugin report "not cached" in every
//! other repo. This module re-asserts user scope for every key cc-loadout
//! manages. It is the Rust port of the retired `lib/registry.sh`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

use crate::profile::config::Profiles;
use crate::util::atomicfile;

/// Outcome of one promotion pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PromoteReport {
    /// Keys moved to `scope: user` by this pass.
    pub promoted: Vec<String>,
    /// Keys named in profiles.json but absent from the registry.
    pub not_installed: Vec<String>,
}

impl PromoteReport {
    /// True when the pass mutated the registry.
    pub fn changed(&self) -> bool {
        !self.promoted.is_empty()
    }
}

/// Every key cc-loadout manages: universal, on-demand, and all profile plugins.
fn managed_keys(cfg: &Profiles) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    keys.extend(cfg.universal.iter().cloned());
    keys.extend(cfg.on_demand.iter().cloned());
    for p in cfg.profiles.values() {
        keys.extend(p.plugins.iter().cloned());
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Ensure every key in `keys` has a `scope: user` entry.
///
/// An existing user-scope entry is left untouched. A key with no entries is
/// reported in `not_installed` and skipped. Otherwise the most-recently-updated
/// entry wins, loses its `projectPath`, becomes `scope: user`, and replaces the
/// entry array. The file is rewritten only when something actually changed, so
/// the common no-op session start performs no I/O beyond one read.
pub fn promote_keys_to_user(registry_path: &Path, keys: &[String]) -> Result<PromoteReport> {
    let mut report = PromoteReport::default();
    if keys.is_empty() {
        return Ok(report);
    }
    let bytes = match std::fs::read(registry_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e).with_context(|| format!("reading {}", registry_path.display())),
    };
    let mut root: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", registry_path.display()))?;

    {
        let plugins = match root.get_mut("plugins").and_then(Value::as_object_mut) {
            Some(p) => p,
            None => return Ok(report),
        };
        for key in keys {
            let entries = match plugins.get_mut(key).and_then(Value::as_array_mut) {
                Some(e) if !e.is_empty() => e,
                _ => {
                    report.not_installed.push(key.clone());
                    continue;
                }
            };
            let has_user = entries
                .iter()
                .any(|e| e.get("scope").and_then(Value::as_str) == Some("user"));
            if has_user {
                continue;
            }
            let mut winner = entries
                .iter()
                .max_by(|a, b| {
                    let a_updated = a.get("lastUpdated").and_then(Value::as_str).unwrap_or("");
                    let b_updated = b.get("lastUpdated").and_then(Value::as_str).unwrap_or("");
                    a_updated.cmp(b_updated)
                })
                .cloned()
                .expect("entries is non-empty");
            if let Some(obj) = winner.as_object_mut() {
                obj.remove("projectPath");
                obj.insert("scope".to_string(), Value::String("user".to_string()));
            }
            *entries = vec![winner];
            report.promoted.push(key.clone());
        }
    }

    if report.changed() {
        // Backup is best-effort insurance; the write proceeds regardless if it fails.
        let _ = std::fs::copy(registry_path, atomicfile::sidecar_backup(registry_path));
        let out = serde_json::to_vec_pretty(&root)?;
        atomicfile::write_atomic(registry_path, &out, 0o644)?;
    }
    Ok(report)
}

/// Promote every key cc-loadout manages.
pub fn promote_all(cfg: &Profiles, registry_path: &Path) -> Result<PromoteReport> {
    promote_keys_to_user(registry_path, &managed_keys(cfg))
}

/// Read-only counterpart of `promote_all`: the managed keys that are installed
/// but lack a user-scope entry. Used by `doctor` without `--fix`.
pub fn keys_needing_promotion(cfg: &Profiles, registry_path: &Path) -> Vec<String> {
    let installed = crate::profile::discover::list_plugins(registry_path);
    let managed = managed_keys(cfg);
    installed
        .into_iter()
        .filter(|p| managed.contains(&p.key) && !p.scopes.iter().any(|s| s == "user"))
        .map(|p| p.key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_registry(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("installed_plugins.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn promotes_the_most_recent_local_entry_and_drops_project_path() {
        let dir = tempfile::tempdir().unwrap();
        let reg = write_registry(
            dir.path(),
            r#"{"version":2,"plugins":{"a@m":[
                {"scope":"local","projectPath":"/old","lastUpdated":"2026-01-01T00:00:00Z"},
                {"scope":"local","projectPath":"/new","lastUpdated":"2026-06-01T00:00:00Z"}
            ]}}"#,
        );
        let report = promote_keys_to_user(&reg, &["a@m".to_string()]).unwrap();
        assert_eq!(report.promoted, vec!["a@m".to_string()]);

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&reg).unwrap()).unwrap();
        let entries = v["plugins"]["a@m"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "the winner replaces the whole array");
        assert_eq!(entries[0]["scope"], "user");
        assert_eq!(entries[0]["lastUpdated"], "2026-06-01T00:00:00Z");
        assert!(entries[0].get("projectPath").is_none());
    }

    #[test]
    fn leaves_an_existing_user_scope_entry_alone_and_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"version":2,"plugins":{"a@m":[{"scope":"user","lastUpdated":"x"}]}}"#;
        let reg = write_registry(dir.path(), body);
        let before = std::fs::read_to_string(&reg).unwrap();

        let report = promote_keys_to_user(&reg, &["a@m".to_string()]).unwrap();
        assert!(report.promoted.is_empty());
        assert!(!report.changed());
        assert_eq!(
            std::fs::read_to_string(&reg).unwrap(),
            before,
            "an unchanged registry must not be rewritten"
        );
    }

    #[test]
    fn reports_keys_absent_from_the_registry_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        let reg = write_registry(dir.path(), r#"{"version":2,"plugins":{}}"#);
        let report = promote_keys_to_user(&reg, &["ghost@m".to_string()]).unwrap();
        assert_eq!(report.not_installed, vec!["ghost@m".to_string()]);
        assert!(report.promoted.is_empty());
    }

    #[test]
    fn missing_registry_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("nope.json");
        let report = promote_keys_to_user(&reg, &["a@m".to_string()]).unwrap();
        assert!(report.promoted.is_empty());
    }

    #[test]
    fn promote_all_covers_universal_on_demand_and_profile_plugins_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let reg = write_registry(
            dir.path(),
            r#"{"version":2,"plugins":{
                "u@m":[{"scope":"local","lastUpdated":"1"}],
                "o@m":[{"scope":"local","lastUpdated":"1"}],
                "p@m":[{"scope":"local","lastUpdated":"1"}]
            }}"#,
        );
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal":["u@m","p@m"],"on_demand":["o@m"],
                "profiles":{"x":{"plugins":["p@m"],"detect":{}}}}"#,
        )
        .unwrap();

        let report = promote_all(&cfg, &reg).unwrap();
        let mut got = report.promoted.clone();
        got.sort();
        assert_eq!(
            got,
            vec!["o@m".to_string(), "p@m".to_string(), "u@m".to_string()]
        );
    }

    #[test]
    fn writes_exactly_one_fixed_name_backup() {
        let dir = tempfile::tempdir().unwrap();
        let reg = write_registry(
            dir.path(),
            r#"{"version":2,"plugins":{"a@m":[{"scope":"local","lastUpdated":"1"}]}}"#,
        );
        promote_keys_to_user(&reg, &["a@m".to_string()]).unwrap();
        // Force a second change so a per-run backup scheme would leave two files.
        std::fs::write(
            &reg,
            r#"{"version":2,"plugins":{"a@m":[{"scope":"local","lastUpdated":"2"}]}}"#,
        )
        .unwrap();
        promote_keys_to_user(&reg, &["a@m".to_string()]).unwrap();

        let baks: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .collect();
        assert_eq!(
            baks.len(),
            1,
            "exactly one fixed-name backup, never a growing set"
        );
    }

    #[test]
    fn propagates_read_errors_other_than_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // A directory path will fail with a non-NotFound error when passed to std::fs::read.
        let result = promote_keys_to_user(dir.path(), &["a@m".to_string()]);
        assert!(result.is_err(), "should propagate the read error");
    }
}
