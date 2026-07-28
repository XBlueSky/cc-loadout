use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::account::paths::ClaudePaths;
use crate::profile::config::Profiles;
use crate::profile::plugins::{desired_plugins, managed_keys};

fn target_path(root: &Path) -> std::path::PathBuf {
    root.join(".claude").join("settings.local.json")
}

/// Merge managed enabledPlugins keys into `<root>/.claude/settings.local.json`.
/// Returns (before_enabled, after_enabled) for diffing.
///
/// Takes the same exclusive flock `on_demand.rs`'s `acquire`/`release`/
/// `release_all` take (via `crate::profile::on_demand::lock_path`) around
/// its own read-modify-write of this file. Without it, a `profile apply`
/// racing an on-demand acquire/release in the same repo could lose
/// whichever wrote last, silently clobbering the other's change with its
/// stale in-memory snapshot.
pub fn apply(root: &Path, cfg: &Profiles, matched: &[String]) -> Result<(Value, Value)> {
    let _lock = crate::util::lock::acquire(&crate::profile::on_demand::lock_path(root))?;

    let target = target_path(root);
    let desired: BTreeSet<String> = desired_plugins(cfg, matched).into_iter().collect();
    let keys = managed_keys(cfg);

    let mut before = Value::Object(Map::new());
    let mut after = Value::Object(Map::new());

    crate::util::jsonmerge::merge_object(&target, 0o644, |settings| {
        before = settings
            .get("enabledPlugins")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));

        let enabled = settings
            .entry("enabledPlugins")
            .or_insert_with(|| Value::Object(Map::new()));
        if !enabled.is_object() {
            *enabled = Value::Object(Map::new());
        }
        let enabled_obj = enabled.as_object_mut().unwrap();
        for key in keys {
            enabled_obj.insert(key.clone(), Value::Bool(desired.contains(&key)));
        }

        after = settings
            .get("enabledPlugins")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(())
    })?;

    Ok((before, after))
}

/// Compute what [`apply`] WOULD write for `root`, without touching disk.
/// Returns `(before, after)` of the `enabledPlugins` object exactly as `apply`
/// would produce them, so a caller can diff for a `--dry-run`. Read-only: takes
/// no lock and creates no file (mirrors `apply`'s managed-key logic so the two
/// never drift).
pub fn preview(root: &Path, cfg: &Profiles, matched: &[String]) -> Result<(Value, Value)> {
    let desired: BTreeSet<String> = desired_plugins(cfg, matched).into_iter().collect();
    let keys = managed_keys(cfg);
    let target = target_path(root);

    // Mirror `jsonmerge::merge_object`'s load step rather than going through
    // `current_enabled`: absent file → `{}`, unparseable → the same parse error,
    // and a non-object ROOT is rejected exactly as the real apply rejects it.
    // (`current_enabled` would silently report "no enabledPlugins" for a root
    // like `[1,2,3]`, making a dry-run promise a write that `apply` refuses.)
    let settings: Value = if target.exists() {
        let bytes =
            std::fs::read(&target).with_context(|| format!("reading {}", target.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", target.display()))?
    } else {
        Value::Object(Map::new())
    };
    if !settings.is_object() {
        bail!("{} is not a JSON object", target.display());
    }

    let before = settings
        .get("enabledPlugins")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    // `apply` resets a non-object `enabledPlugins` to `{}` before inserting; a
    // preview must do the same so a corrupt value doesn't skew the diff.
    let mut after_obj = before.as_object().cloned().unwrap_or_default();
    for key in keys {
        after_obj.insert(key.clone(), Value::Bool(desired.contains(&key)));
    }

    Ok((before, Value::Object(after_obj)))
}

/// The global plugin settings file: `<config_dir>/settings.json`, where the
/// config dir is the directory holding `.credentials.json` (respects
/// `$CLAUDE_CONFIG_DIR`).
pub fn global_settings_path(claude: &ClaudePaths) -> std::path::PathBuf {
    claude
        .credentials
        .parent()
        .map(|d| d.join("settings.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("settings.json"))
}

/// Currently-`true` plugin keys in the global settings.json, sorted. Empty when
/// the file or the `enabledPlugins` key is absent.
#[allow(dead_code)]
pub fn read_global_enabled(settings_path: &Path) -> Result<Vec<String>> {
    if !settings_path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(settings_path)
        .with_context(|| format!("reading {}", settings_path.display()))?;
    let v: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", settings_path.display()))?;
    let mut keys: Vec<String> = v
        .get("enabledPlugins")
        .and_then(|e| e.as_object())
        .map(|o| {
            o.iter()
                .filter(|(_, val)| val.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    Ok(keys)
}

/// Set the GLOBAL `enabledPlugins`: managed universal keys → `true`, other
/// managed keys → `false`; unmanaged keys and all other settings preserved.
/// Returns `(before, after)` of the `enabledPlugins` object for diffing.
pub fn apply_global(settings_path: &Path, cfg: &Profiles) -> Result<(Value, Value)> {
    let universal: BTreeSet<String> = cfg.universal.iter().cloned().collect();
    let keys = managed_keys(cfg);

    let mut before = Value::Object(Map::new());
    let mut after = Value::Object(Map::new());

    crate::util::jsonmerge::merge_object(settings_path, 0o644, |settings| {
        before = settings
            .get("enabledPlugins")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));

        let enabled = settings
            .entry("enabledPlugins")
            .or_insert_with(|| Value::Object(Map::new()));
        if !enabled.is_object() {
            *enabled = Value::Object(Map::new());
        }
        let enabled_obj = enabled.as_object_mut().unwrap();
        for key in keys {
            enabled_obj.insert(key.clone(), Value::Bool(universal.contains(&key)));
        }

        after = settings
            .get("enabledPlugins")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(())
    })?;

    Ok((before, after))
}

/// Return the current `enabledPlugins` object, or None if the file/key is missing.
pub fn current_enabled(root: &Path) -> Result<Option<Value>> {
    let target = target_path(root);
    if !target.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&target).with_context(|| format!("reading {}", target.display()))?;
    let v: Value =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", target.display()))?;
    Ok(v.get("enabledPlugins").cloned())
}

/// Enabled (`true`) plugin keys from settings.local.json, sorted. Empty when the
/// file or the `enabledPlugins` key is absent.
pub fn enabled_keys(root: &Path) -> Result<Vec<String>> {
    let mut keys: Vec<String> = match current_enabled(root)? {
        Some(v) => v
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, val)| val.as_bool() == Some(true))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    };
    keys.sort();
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profiles;
    use serde_json::json;

    fn cfg() -> Profiles {
        serde_json::from_str(
            r#"{
            "universal": ["u@m"],
            "profiles": {
                "a": {"plugins": ["a1@m"], "detect": {}},
                "b": {"plugins": ["b1@m"], "detect": {}}
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn apply_sets_managed_true_false_and_preserves_others() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(
            &target,
            r#"{"theme":"dark","enabledPlugins":{"ondemand@m":true}}"#,
        )
        .unwrap();

        let (_before, after) = apply(dir.path(), &cfg(), &["a".to_string()]).unwrap();
        assert_eq!(after["u@m"], json!(true));
        assert_eq!(after["a1@m"], json!(true));
        assert_eq!(after["b1@m"], json!(false));
        assert_eq!(after["ondemand@m"], json!(true));

        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(on_disk["theme"], json!("dark"));
    }

    #[test]
    fn apply_creates_file_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let (_b, after) = apply(dir.path(), &cfg(), &[]).unwrap();
        assert_eq!(after["u@m"], json!(true));
        assert_eq!(after["a1@m"], json!(false));
        assert!(dir
            .path()
            .join(".claude")
            .join("settings.local.json")
            .exists());
    }

    #[test]
    fn preview_computes_apply_diff_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude").join("settings.local.json");

        // No file yet: preview reports the would-be state but writes nothing.
        let (before, after) = preview(dir.path(), &cfg(), &["a".to_string()]).unwrap();
        assert_eq!(before, json!({}));
        assert_eq!(after["u@m"], json!(true));
        assert_eq!(after["a1@m"], json!(true));
        assert_eq!(after["b1@m"], json!(false));
        assert!(
            !target.exists(),
            "preview must not create settings.local.json"
        );
    }

    #[test]
    fn preview_matches_apply_and_leaves_disk_untouched() {
        // Parity: preview's (before, after) must equal what apply would write,
        // and running preview after apply must report an empty diff (in sync).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude").join("settings.local.json"),
            r#"{"theme":"dark","enabledPlugins":{"ondemand@m":true}}"#,
        )
        .unwrap();
        let matched = ["a".to_string()];

        let (p_before, p_after) = preview(dir.path(), &cfg(), &matched).unwrap();
        let (a_before, a_after) = apply(dir.path(), &cfg(), &matched).unwrap();
        assert_eq!(p_before, a_before, "preview.before == apply.before");
        assert_eq!(p_after, a_after, "preview.after == apply.after");

        // After apply, a fresh preview shows no change (before == after).
        let (before2, after2) = preview(dir.path(), &cfg(), &matched).unwrap();
        assert_eq!(before2, after2, "in-sync repo previews as no-change");
    }

    #[test]
    fn preview_rejects_what_apply_rejects() {
        // A dry-run must never promise a write that `apply` would refuse. Both
        // a non-object ROOT and unparseable JSON must fail in preview exactly
        // as they fail in apply — otherwise `--dry-run` under-reports breakage.
        for body in [&b"[1,2,3]"[..], &b""[..], &b"{"[..]] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join(".claude").join("settings.local.json");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, body).unwrap();

            let p = preview(dir.path(), &cfg(), &[]);
            let a = apply(dir.path(), &cfg(), &[]);
            assert!(
                p.is_err(),
                "preview must reject {:?} like apply does",
                String::from_utf8_lossy(body)
            );
            assert!(
                a.is_err(),
                "sanity: apply rejects {:?}",
                String::from_utf8_lossy(body)
            );
            assert_eq!(
                p.unwrap_err().to_string(),
                a.unwrap_err().to_string(),
                "preview and apply must report the SAME error for {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn current_enabled_reads_or_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(current_enabled(dir.path()).unwrap().is_none());
        apply(dir.path(), &cfg(), &[]).unwrap();
        assert!(current_enabled(dir.path()).unwrap().is_some());
    }

    #[test]
    fn global_settings_path_respects_config_dir() {
        use crate::account::paths;
        let p = paths::resolve(std::path::Path::new("/home/u"), None);
        assert_eq!(
            super::global_settings_path(&p),
            std::path::Path::new("/home/u/.claude/settings.json")
        );
    }

    #[test]
    fn read_global_enabled_lists_true_keys_or_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("settings.json");
        assert!(read_global_enabled(&s).unwrap().is_empty()); // absent file
        std::fs::write(&s, r#"{"enabledPlugins":{"on@m":true,"off@m":false}}"#).unwrap();
        assert_eq!(read_global_enabled(&s).unwrap(), vec!["on@m".to_string()]);
    }

    #[test]
    fn apply_global_keeps_universal_disables_profile_preserves_rest() {
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("settings.json");
        std::fs::write(
            &s,
            r#"{"theme":"dark","enabledPlugins":{"u@m":true,"a1@m":true,"other@m":true}}"#,
        )
        .unwrap();

        let (before, after) = apply_global(&s, &cfg()).unwrap();
        // managed: u@m (universal) -> true; a1@m,b1@m (profile-specific) -> false.
        assert_eq!(after["u@m"], json!(true));
        assert_eq!(after["a1@m"], json!(false));
        assert_eq!(after["b1@m"], json!(false));
        // unmanaged key untouched; before reflects prior state.
        assert_eq!(after["other@m"], json!(true));
        assert_eq!(before["a1@m"], json!(true));
        // other settings keys preserved.
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        assert_eq!(on_disk["theme"], json!("dark"));
    }

    #[test]
    fn apply_locks_against_concurrent_on_demand_acquire() {
        // Regression test for the lost-update race: `apply()` and
        // `on_demand::acquire()` both do a full read-modify-write of
        // `settings.local.json`'s `enabledPlugins` object. Without a shared
        // lock, whichever finishes last silently clobbers the other's
        // change with its own stale in-memory snapshot. Stress across
        // iterations (fresh tempdir each time) since a single race is
        // timing-dependent — mirrors on_demand.rs's
        // `concurrent_acquire_from_two_sessions_preserves_both_holders`.
        for _ in 0..50 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let (root1, barrier1) = (root.clone(), barrier.clone());
            let cfg1 = cfg();
            let t1 = std::thread::spawn(move || {
                barrier1.wait();
                apply(&root1, &cfg1, &["a".to_string()]).unwrap();
            });
            let (root2, barrier2) = (root.clone(), barrier.clone());
            let t2 = std::thread::spawn(move || {
                barrier2.wait();
                crate::profile::on_demand::acquire(&root2, "sess-1", "pixijs@x").unwrap();
            });
            t1.join().unwrap();
            t2.join().unwrap();

            let enabled = current_enabled(&root).unwrap().unwrap();
            assert_eq!(
                enabled["a1@m"],
                json!(true),
                "lost update: apply()'s write to a1@m was clobbered by a \
                 concurrent on_demand::acquire"
            );
            assert_eq!(
                enabled["pixijs@x"],
                json!(true),
                "lost update: on_demand::acquire's write to pixijs@x was \
                 clobbered by a concurrent apply()"
            );
        }
    }
}
