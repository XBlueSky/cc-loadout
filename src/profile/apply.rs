use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::account::paths::ClaudePaths;
use crate::profile::config::Profiles;
use crate::profile::managed;
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
    let drop = orphans_to_drop(root, &keys)?;

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
        // Clean up first: a key cc-loadout has stopped managing is dropped
        // outright, so nothing is left behind for a future config to inherit.
        for key in &drop {
            enabled_obj.remove(key);
        }
        for key in &keys {
            enabled_obj.insert(key.clone(), Value::Bool(desired.contains(key)));
        }

        after = settings
            .get("enabledPlugins")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        Ok(())
    })?;

    // Only after the settings write succeeded, so a failed apply never records
    // ownership of keys it did not actually write. Still under `_lock`.
    managed::save(root, &keys)?;

    Ok((before, after))
}

/// The managed-key tombstone lookup shared by [`apply`] and [`preview`] so the
/// two can never disagree about what a run would clean up.
///
/// A repo with no record predates the tombstone: its history is unknown, so
/// nothing may be removed and the run only seeds the record for next time.
fn orphans_to_drop(root: &Path, now_managed: &[String]) -> Result<Vec<String>> {
    let Some(previous) = managed::load(root)? else {
        return Ok(Vec::new());
    };
    let now: BTreeSet<String> = now_managed.iter().cloned().collect();
    let held = crate::profile::on_demand::held_keys(root)?;
    Ok(managed::orphans(&previous, &now, &held))
}

/// Compute what [`apply`] WOULD write for `root`, without touching disk.
/// Returns `(before, after)` of the `enabledPlugins` object exactly as `apply`
/// would produce them, so a caller can diff for a `--dry-run`. Read-only: takes
/// no lock and creates no file (mirrors `apply`'s managed-key logic so the two
/// never drift).
pub fn preview(root: &Path, cfg: &Profiles, matched: &[String]) -> Result<(Value, Value)> {
    let desired: BTreeSet<String> = desired_plugins(cfg, matched).into_iter().collect();
    let keys = managed_keys(cfg);
    let drop = orphans_to_drop(root, &keys)?;
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
    for key in &drop {
        after_obj.remove(key);
    }
    for key in &keys {
        after_obj.insert(key.clone(), Value::Bool(desired.contains(key)));
    }

    Ok((before, Value::Object(after_obj)))
}

/// Remove the named `enabledPlugins` keys from `<root>/.claude/settings.local.json`,
/// returning the ones actually removed (sorted, deduped).
///
/// This is the one-shot cleanup for keys fossilised BEFORE the managed-key
/// tombstone existed (see `managed.rs`). Deliberately dumb: it removes exactly
/// the keys it is told to and never infers which keys "look" orphaned — the
/// unmanaged half of `enabledPlugins` is also where a user's own hand-toggles
/// live, and guessing there would break the very invariant `apply` protects.
///
/// `dry_run` reports the same list without touching the file. A repo with no
/// settings file is skipped rather than created: `prune --all` walks every repo
/// under `scan_roots` and must not leave a file behind in the ones that had none.
pub fn prune(root: &Path, keys: &[String], dry_run: bool) -> Result<Vec<String>> {
    let target = target_path(root);
    if !target.exists() {
        return Ok(Vec::new());
    }

    // Cheap read first so the overwhelming majority of repos — the ones holding
    // none of these keys — are neither locked nor rewritten.
    let present: Vec<String> = {
        let bytes =
            std::fs::read(&target).with_context(|| format!("reading {}", target.display()))?;
        let v: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", target.display()))?;
        match v.get("enabledPlugins").and_then(|e| e.as_object()) {
            Some(obj) => keys
                .iter()
                .filter(|k| obj.contains_key(k.as_str()))
                .cloned()
                .collect::<BTreeSet<String>>()
                .into_iter()
                .collect(),
            None => Vec::new(),
        }
    };
    if dry_run || present.is_empty() {
        return Ok(present);
    }

    // Same lock `apply` and the on-demand acquire/release take: this is another
    // full read-modify-write of the same file.
    let _lock = crate::util::lock::acquire(&crate::profile::on_demand::lock_path(root))?;
    let mut removed = BTreeSet::new();
    crate::util::jsonmerge::merge_object(&target, 0o644, |settings| {
        if let Some(enabled) = settings
            .get_mut("enabledPlugins")
            .and_then(|e| e.as_object_mut())
        {
            for k in keys {
                if enabled.remove(k).is_some() {
                    removed.insert(k.clone());
                }
            }
        }
        Ok(())
    })?;
    Ok(removed.into_iter().collect())
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

/// On-demand keys that are `true` in the GLOBAL settings.json — the state that
/// silently enables them in every repo.
///
/// `enabledPlugins` merges per key, so a key a repo's `settings.local.json`
/// does not mention inherits the global value. An unheld on-demand key's
/// correct state is *absent* from a repo (`on_demand::release` removes the key
/// rather than writing `false`; see `managed::orphans`), so a global `true` is
/// inherited everywhere: the plugin loads in every repo, `acquire` has nothing
/// to turn on and `release` nothing to revert. `apply_global` cannot correct
/// this, because `managed_keys()` deliberately excludes the on-demand pool —
/// it never writes these keys at all, leaving whatever Claude Code wrote when
/// the plugin was installed.
///
/// A key that is ALSO universal or profile-specific is excluded: nothing
/// validates that the pools are disjoint, and for a key in both `apply_global`
/// owns the global value — demoting it here would only make the two fight.
pub fn demotable_on_demand_keys(settings_path: &Path, cfg: &Profiles) -> Result<Vec<String>> {
    let globally_true: BTreeSet<String> = read_global_enabled(settings_path)?.into_iter().collect();
    let managed: BTreeSet<String> = managed_keys(cfg).into_iter().collect();
    // Through a BTreeSet, which sorts and dedupes in one step, so the report is
    // stable regardless of the order the pool happens to be written in.
    Ok(cfg
        .on_demand
        .iter()
        .filter(|k| globally_true.contains(*k) && !managed.contains(*k))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect())
}

/// Write `false` for every key `demotable_on_demand_keys` reports, returning
/// the keys changed.
///
/// A clean run does not write the file at all. This is `~/.claude/settings.json`
/// — the one file the user does not expect a tool to touch unasked — and a
/// diagnostic must not rewrite it just to report that nothing was wrong.
///
/// Safe to run while a session holds one of these keys: `acquire` writes the
/// key `true` in the repo's `settings.local.json`, and the repo layer wins.
pub fn demote_on_demand_global(settings_path: &Path, cfg: &Profiles) -> Result<Vec<String>> {
    let keys = demotable_on_demand_keys(settings_path, cfg)?;
    if keys.is_empty() {
        return Ok(keys);
    }
    crate::util::jsonmerge::merge_object(settings_path, 0o644, |settings| {
        let enabled = settings
            .entry("enabledPlugins")
            .or_insert_with(|| Value::Object(Map::new()));
        if !enabled.is_object() {
            *enabled = Value::Object(Map::new());
        }
        let enabled_obj = enabled.as_object_mut().unwrap();
        for key in &keys {
            enabled_obj.insert(key.clone(), Value::Bool(false));
        }
        Ok(())
    })?;
    Ok(keys)
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
    fn a_hand_toggled_key_survives_apply_even_once_a_tombstone_exists() {
        // Guard for invariant #2 now that `apply` can delete: the tombstone is
        // the ONLY licence to remove. A key cc-loadout never wrote — a user's
        // own toggle sitting in the unmanaged half of enabledPlugins — must
        // still be untouchable. Fails if `orphans` ever grows to consider the
        // whole file rather than what the previous apply recorded.
        let dir = tempfile::tempdir().unwrap();
        apply(dir.path(), &cfg(), &["a".to_string()]).unwrap(); // seeds the record

        crate::util::jsonmerge::merge_object(&target_path(dir.path()), 0o644, |s| {
            s["enabledPlugins"]["myown@m"] = json!(true);
            Ok(())
        })
        .unwrap();

        let (_before, after) = apply(dir.path(), &cfg(), &["a".to_string()]).unwrap();
        assert_eq!(
            after["myown@m"],
            json!(true),
            "a key cc-loadout never managed must survive; got {after:?}"
        );
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
    fn apply_removes_a_key_that_left_the_managed_set() {
        // Regression: a plugin dropped from profiles.json (e.g. uninstalled)
        // fell out of `managed_keys()`, so `apply`'s `for key in keys` loop
        // stopped touching it and its last-written `true` was fossilised in
        // every repo forever. `apply` must remember what it managed last time.
        let dir = tempfile::tempdir().unwrap();

        let old: Profiles =
            serde_json::from_str(r#"{"universal": ["u@m", "gone@m"], "profiles": {}}"#).unwrap();
        apply(dir.path(), &old, &[]).unwrap();
        assert_eq!(
            current_enabled(dir.path()).unwrap().unwrap()["gone@m"],
            json!(true),
            "sanity: the first apply enables gone@m"
        );

        // `gone@m` is no longer named anywhere in the config.
        let (_before, after) = apply(dir.path(), &cfg(), &[]).unwrap();
        assert!(
            after.get("gone@m").is_none(),
            "a key that left the managed set must be dropped, not fossilised; got {after:?}"
        );
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
    fn prune_removes_only_the_named_keys_and_reports_them() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(
            &target,
            r#"{"theme":"dark","enabledPlugins":{"serena@m":true,"keep@m":true}}"#,
        )
        .unwrap();

        let removed = prune(dir.path(), &["serena@m".to_string()], false).unwrap();
        assert_eq!(removed, vec!["serena@m".to_string()]);

        let on_disk: Value = serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert!(on_disk["enabledPlugins"].get("serena@m").is_none());
        assert_eq!(on_disk["enabledPlugins"]["keep@m"], json!(true));
        assert_eq!(on_disk["theme"], json!("dark"), "other settings preserved");
    }

    #[test]
    fn prune_dry_run_reports_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let body = r#"{"enabledPlugins":{"serena@m":true}}"#;
        std::fs::write(&target, body).unwrap();

        let removed = prune(dir.path(), &["serena@m".to_string()], true).unwrap();
        assert_eq!(removed, vec!["serena@m".to_string()]);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            body,
            "a dry run must leave the file byte-identical"
        );
    }

    #[test]
    fn prune_reports_nothing_for_a_key_that_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, r#"{"enabledPlugins":{"keep@m":true}}"#).unwrap();

        let removed = prune(dir.path(), &["serena@m".to_string()], false).unwrap();
        assert!(removed.is_empty(), "got {removed:?}");
    }

    #[test]
    fn prune_never_creates_a_settings_file_in_a_repo_that_has_none() {
        // `prune --all` walks a thousand repos; it must not leave a settings
        // file behind in the ones that never had one.
        let dir = tempfile::tempdir().unwrap();
        let removed = prune(dir.path(), &["serena@m".to_string()], false).unwrap();
        assert!(removed.is_empty());
        assert!(!dir
            .path()
            .join(".claude")
            .join("settings.local.json")
            .exists());
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

    fn cfg_on_demand() -> Profiles {
        // `u@m` sits in BOTH `universal` and `on_demand`: nothing validates
        // that the pools are disjoint, and a key in both is a managed key
        // whose global value `apply_global` owns.
        serde_json::from_str(
            r#"{
            "universal": ["u@m"],
            "profiles": {
                "a": {"plugins": ["a1@m"], "detect": {}}
            },
            "on_demand": ["od@m", "off@m", "absent@m", "u@m"]
        }"#,
        )
        .unwrap()
    }

    fn write_global_settings(dir: &Path) -> std::path::PathBuf {
        let s = dir.join("settings.json");
        std::fs::write(
            &s,
            r#"{"theme":"dark","enabledPlugins":{"od@m":true,"off@m":false,"u@m":true,"a1@m":false,"other@m":true}}"#,
        )
        .unwrap();
        s
    }

    #[test]
    fn demotable_on_demand_keys_lists_only_globally_true_unmanaged_on_demand_keys() {
        // od@m: on-demand and globally true -> the leak.
        // off@m: on-demand but already false. absent@m: not in the file at all.
        // u@m: on-demand AND universal, so a managed key. a1@m: profile-specific.
        // other@m: globally true but not an on-demand key at all.
        let dir = tempfile::tempdir().unwrap();
        let s = write_global_settings(dir.path());
        assert_eq!(
            demotable_on_demand_keys(&s, &cfg_on_demand()).unwrap(),
            vec!["od@m".to_string()]
        );
    }

    #[test]
    fn demotable_on_demand_keys_is_empty_when_the_settings_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("settings.json");
        assert!(demotable_on_demand_keys(&s, &cfg_on_demand())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn demote_on_demand_global_writes_false_and_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let s = write_global_settings(dir.path());
        assert_eq!(
            demote_on_demand_global(&s, &cfg_on_demand()).unwrap(),
            vec!["od@m".to_string()]
        );
        let v: Value = serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        assert_eq!(v["enabledPlugins"]["od@m"], json!(false));
        assert_eq!(
            v["enabledPlugins"]["other@m"],
            json!(true),
            "a key outside every pool must survive"
        );
        assert_eq!(
            v["theme"],
            json!("dark"),
            "other settings keys must survive"
        );
    }

    #[test]
    fn demote_on_demand_global_leaves_a_key_that_is_also_universal_enabled() {
        // Without the managed-key guard `doctor --fix` and `apply_global`
        // would fight over `u@m` forever: one writes false, the other true.
        let dir = tempfile::tempdir().unwrap();
        let s = write_global_settings(dir.path());
        demote_on_demand_global(&s, &cfg_on_demand()).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&s).unwrap()).unwrap();
        assert_eq!(v["enabledPlugins"]["u@m"], json!(true));
    }

    #[test]
    fn demote_on_demand_global_writes_nothing_when_no_key_leaks() {
        // The file this touches is `~/.claude/settings.json`. A clean run must
        // leave it byte-identical rather than rewrite it with the same content.
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("settings.json");
        let body = r#"{"enabledPlugins":{"off@m":false,"u@m":true}}"#;
        std::fs::write(&s, body).unwrap();
        assert!(demote_on_demand_global(&s, &cfg_on_demand())
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(&s).unwrap(),
            body,
            "nothing leaked, so nothing may be written"
        );
    }
}
