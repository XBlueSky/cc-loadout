//! The managed-key tombstone: a per-repo record of which `enabledPlugins` keys
//! the LAST `apply` owned.
//!
//! `plugins::managed_keys()` is derived purely from the CURRENT config, so it
//! can say "these are mine" but never "these USED to be mine". A plugin dropped
//! from `profiles.json` therefore fell out of the managed set and `apply` stopped
//! touching it — fossilising its last-written value in every repo forever (and,
//! for a `true`, silently reinstalling the plugin at local scope each time the
//! repo was opened).
//!
//! This module is the missing memory. It lives beside `on-demand.json` in the
//! `.claude/.cc-loadout/` directory cc-loadout already owns, and is written under
//! the same flock `apply` and `on_demand` share, so the record and the
//! `settings.local.json` it describes move together.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What the last `apply` managed in this repo.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ManagedRecord {
    /// `plugins::managed_keys()` as of that apply — every key cc-loadout wrote,
    /// whether it wrote `true` or `false`.
    #[serde(default)]
    pub keys: Vec<String>,
}

fn record_path(root: &Path) -> PathBuf {
    root.join(".claude")
        .join(".cc-loadout")
        .join("managed.json")
}

/// The keys the previous `apply` managed, or `None` when this repo has no record
/// yet. `None` is NOT the same as `Some(vec![])`: a repo predating the tombstone
/// has unknown history and nothing may be removed from it, whereas an empty
/// record means the last apply genuinely managed nothing.
pub fn load(root: &Path) -> Result<Option<Vec<String>>> {
    let p = record_path(root);
    if !p.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
    let rec: ManagedRecord =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", p.display()))?;
    Ok(Some(rec.keys))
}

/// Record `keys` as this repo's managed set. Call only AFTER the matching
/// `settings.local.json` write succeeded, so a failed apply never records a lie.
pub fn save(root: &Path, keys: &[String]) -> Result<()> {
    let rec = ManagedRecord {
        keys: keys.to_vec(),
    };
    let body = serde_json::to_vec_pretty(&rec)?;
    crate::util::atomicfile::write_atomic(&record_path(root), &body, 0o644)
}

/// Keys `apply` must clean out of `enabledPlugins`: ones cc-loadout wrote on a
/// previous run but no longer manages.
///
/// - `previous`: the tombstone — what the last apply managed in this repo.
/// - `now_managed`: `plugins::managed_keys()` for the current config.
/// - `held`: on-demand keys with at least one live session holder.
///
/// `previous` only ever contains keys cc-loadout itself wrote, so dropping one
/// it no longer manages is always the tool cleaning up after itself — including
/// a plugin that moved into `on_demand`, whose correct unheld state is "absent",
/// exactly as a never-acquired on-demand key looks. The one thing that must not
/// be yanked is a key some session is holding open right now, hence `held`.
///
/// Collected through a `BTreeSet`, which sorts and dedupes in one step.
pub fn orphans(
    previous: &[String],
    now_managed: &BTreeSet<String>,
    held: &BTreeSet<String>,
) -> Vec<String> {
    previous
        .iter()
        .filter(|k| !now_managed.contains(*k) && !held.contains(*k))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn load_is_none_before_any_apply_and_some_after_save() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load(dir.path()).unwrap(),
            None,
            "a repo with no record has unknown history, not an empty one"
        );

        save(dir.path(), &["a@m".to_string(), "b@m".to_string()]).unwrap();
        assert_eq!(
            load(dir.path()).unwrap(),
            Some(vec!["a@m".to_string(), "b@m".to_string()])
        );
    }

    #[test]
    fn save_writes_beside_the_on_demand_state_not_into_settings() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &["a@m".to_string()]).unwrap();
        assert!(dir
            .path()
            .join(".claude")
            .join(".cc-loadout")
            .join("managed.json")
            .exists());
        assert!(
            !dir.path()
                .join(".claude")
                .join("settings.local.json")
                .exists(),
            "the tombstone must not touch Claude Code's own settings file"
        );
    }

    #[test]
    fn an_empty_record_is_distinct_from_no_record() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &[]).unwrap();
        assert_eq!(load(dir.path()).unwrap(), Some(Vec::new()));
    }

    // ---- orphan-selection policy ----

    #[test]
    fn orphans_are_previously_managed_keys_the_config_no_longer_names() {
        // The serena case: it was universal, it is now named nowhere.
        let got = orphans(
            &["u@m".to_string(), "serena@m".to_string()],
            &set(&["u@m", "a1@m"]),
            &set(&[]),
        );
        assert_eq!(got, vec!["serena@m".to_string()]);
    }

    #[test]
    fn a_key_still_managed_is_never_an_orphan() {
        // Still named by the config -> the normal write loop owns it; dropping
        // it here would delete a key apply is about to set.
        let got = orphans(&["u@m".to_string()], &set(&["u@m"]), &set(&[]));
        assert!(got.is_empty(), "got {got:?}");
    }

    #[test]
    fn a_key_a_live_session_holds_on_demand_is_never_an_orphan() {
        // Moving a plugin from `universal` into `on_demand` makes it unmanaged,
        // but yanking it mid-session would break the acquire that is holding it.
        let got = orphans(&["pixijs@x".to_string()], &set(&[]), &set(&["pixijs@x"]));
        assert!(got.is_empty(), "got {got:?}");
    }

    #[test]
    fn orphans_are_sorted_and_deduped() {
        let got = orphans(
            &["z@m".to_string(), "a@m".to_string(), "a@m".to_string()],
            &set(&[]),
            &set(&[]),
        );
        assert_eq!(got, vec!["a@m".to_string(), "z@m".to_string()]);
    }
}
