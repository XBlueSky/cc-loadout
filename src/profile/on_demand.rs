//! Session-scoped, refcounted plugin enablement for `Profiles.on_demand` keys.
//! See `docs/superpowers/specs/2026-07-03-on-demand-plugins-design.md`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct OnDemandEntry {
    /// `enabledPlugins[key]` before the first acquire. `None` means the key
    /// was absent — release must remove it, not write `false`.
    #[serde(default)]
    pub prior_value: Option<bool>,
    /// session_ids currently holding this key open (sorted, deduped).
    #[serde(default)]
    pub holders: Vec<String>,
}

pub type OnDemandState = BTreeMap<String, OnDemandEntry>;

fn state_path(root: &Path) -> PathBuf {
    root.join(".claude")
        .join(".cc-loadout")
        .join("on-demand.json")
}

fn settings_path(root: &Path) -> PathBuf {
    root.join(".claude").join("settings.local.json")
}

/// Visible to `apply.rs` so `apply()` (a full read-modify-write of the same
/// `settings.local.json` this module's `acquire`/`release`/`release_all`
/// lock around) can take the same lock rather than racing them. Do not
/// duplicate this path-computation logic elsewhere — reuse this function.
pub(super) fn lock_path(root: &Path) -> PathBuf {
    root.join(".claude")
        .join(".cc-loadout")
        .join("on-demand.lock")
}

pub fn load_state(root: &Path) -> Result<OnDemandState> {
    let p = state_path(root);
    if !p.exists() {
        return Ok(OnDemandState::new());
    }
    let bytes = std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", p.display()))
}

/// Keys with at least one live session holder in `root`. `apply` consults this
/// so cleaning up a plugin that moved out of the managed set never yanks one an
/// `acquire` is currently holding open.
pub fn held_keys(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    Ok(load_state(root)?
        .into_iter()
        .filter(|(_, e)| !e.holders.is_empty())
        .map(|(k, _)| k)
        .collect())
}

fn save_state(root: &Path, state: &OnDemandState) -> Result<()> {
    let body = serde_json::to_vec_pretty(state)?;
    crate::util::atomicfile::write_atomic(&state_path(root), &body, 0o644)
}

fn write_enabled(root: &Path, key: &str, value: Option<bool>) -> Result<()> {
    crate::util::jsonmerge::merge_object(&settings_path(root), 0o644, |settings| {
        let enabled = settings
            .entry("enabledPlugins")
            .or_insert_with(|| Value::Object(Map::new()));
        if !enabled.is_object() {
            *enabled = Value::Object(Map::new());
        }
        let obj = enabled.as_object_mut().unwrap();
        match value {
            Some(v) => {
                obj.insert(key.to_string(), Value::Bool(v));
            }
            None => {
                obj.remove(key);
            }
        }
        Ok(())
    })
}

/// Add `session_id` as a holder of `key` in `root`, snapshotting the prior
/// `enabledPlugins[key]` value on first acquire, then write `enabledPlugins[key]
/// = true`. Idempotent — acquiring twice in the same session just confirms
/// membership without touching `prior_value` again.
///
/// Holds an exclusive flock on `lock_path(root)` for the whole
/// load-state -> mutate -> write-enabled -> save-state sequence so that two
/// sessions acquiring the same key concurrently serialize instead of
/// racing a read-modify-write on `on-demand.json` (the second `save_state`
/// would otherwise silently clobber the first session's holder entry).
pub fn acquire(root: &Path, session_id: &str, key: &str) -> Result<()> {
    let _lock = crate::util::lock::acquire(&lock_path(root))?;

    let mut state = load_state(root)?;
    if !state.contains_key(key) {
        let prior = crate::profile::apply::current_enabled(root)?
            .and_then(|v| v.get(key).and_then(|b| b.as_bool()));
        state.insert(
            key.to_string(),
            OnDemandEntry {
                prior_value: prior,
                holders: Vec::new(),
            },
        );
    }
    let entry = state.get_mut(key).unwrap();
    if !entry.holders.iter().any(|h| h == session_id) {
        entry.holders.push(session_id.to_string());
        entry.holders.sort();
    }
    write_enabled(root, key, Some(true))?;
    save_state(root, &state)
}

/// `release`'s core logic against an already-loaded, in-memory `state`. Does
/// not lock, load, or save — callers (`release`, `release_all`) own the
/// surrounding critical section so this can be reused inside a single lock
/// acquisition without a public function re-entering the lock.
fn release_locked(
    state: &mut OnDemandState,
    root: &Path,
    session_id: &str,
    key: &str,
    force: bool,
) -> Result<()> {
    let Some(entry) = state.get_mut(key) else {
        return Ok(());
    };
    if !force {
        entry.holders.retain(|h| h != session_id);
    }
    if force || entry.holders.is_empty() {
        let prior = entry.prior_value;
        state.remove(key);
        write_enabled(root, key, prior)?;
    }
    Ok(())
}

/// Remove `session_id` as a holder of `key`. If no holders remain (or `force`),
/// restore `enabledPlugins[key]` to its pre-acquire value and drop the state
/// entry. No-op if `key` has no state entry.
///
/// Locked the same way as `acquire` (see its doc comment) — this function
/// must remain independently correct when called on its own, not just from
/// `release_all`.
pub fn release(root: &Path, session_id: &str, key: &str, force: bool) -> Result<()> {
    let _lock = crate::util::lock::acquire(&lock_path(root))?;

    let mut state = load_state(root)?;
    release_locked(&mut state, root, session_id, key, force)?;
    save_state(root, &state)
}

/// Release every key `session_id` holds in `root` (the SessionEnd hook calls
/// this — a session may have acquired more than one on-demand plugin).
///
/// Takes the lock once around the whole load -> mutate-every-key -> save
/// sequence rather than once per key (fewer lock/unlock cycles, and — more
/// importantly — it prevents a concurrent single-key `release` call from
/// interleaving mid-loop and observing a half-released state). It calls
/// `release_locked` directly instead of the public `release()` to avoid
/// re-entering the flock while already holding it (a second
/// `lock_exclusive()` on the same path from this process would deadlock).
pub fn release_all(root: &Path, session_id: &str) -> Result<()> {
    let _lock = crate::util::lock::acquire(&lock_path(root))?;

    let mut state = load_state(root)?;
    let keys: Vec<String> = state
        .iter()
        .filter(|(_, e)| e.holders.iter().any(|h| h == session_id))
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        release_locked(&mut state, root, session_id, &k, false)?;
    }
    save_state(root, &state)
}

use crate::profile::config::Profiles;
use anyhow::bail;

/// `cc-loadout profile on-demand acquire <key>`. Validates `key` is listed in
/// `cfg.on_demand`, reads the session id from `$CC_LOADOUT_SESSION_ID`.
pub fn cli_acquire(cwd: &Path, cfg: &Profiles, key: &str) -> Result<()> {
    if !cfg.on_demand.iter().any(|k| k == key) {
        bail!(
            "'{key}' is not in on_demand. Add it first (profile edit -> Assign -> On-demand). \
             Known on_demand keys: {}",
            cfg.on_demand.join(", ")
        );
    }
    let session_id = std::env::var("CC_LOADOUT_SESSION_ID").context(
        "CC_LOADOUT_SESSION_ID not set — the cc-loadout SessionStart hook must have run \
         in this session first",
    )?;
    acquire(cwd, &session_id, key)?;
    println!(
        "acquired '{key}' in {} — run /reload-plugins to use it now",
        cwd.display()
    );
    Ok(())
}

/// `cc-loadout profile on-demand release [<key>] [--session-id ID] [--all] [--force]`.
pub fn cli_release(
    cwd: &Path,
    key: Option<String>,
    session_id: Option<String>,
    all: bool,
    force: bool,
) -> Result<()> {
    let session_id = match session_id {
        Some(s) => s,
        None => std::env::var("CC_LOADOUT_SESSION_ID")
            .context("CC_LOADOUT_SESSION_ID not set and no --session-id given")?,
    };
    if all {
        release_all(cwd, &session_id)?;
        println!(
            "released all on-demand holds for session {session_id} in {}",
            cwd.display()
        );
        return Ok(());
    }
    let key = key.context("release requires a <key>, or pass --all")?;
    release(cwd, &session_id, &key, force)?;
    println!("released '{key}' in {}", cwd.display());
    Ok(())
}

#[derive(Serialize)]
struct OnDemandListEntry {
    key: String,
    live_holders: usize,
}

/// Wraps the list in a named field, matching the envelope convention used
/// elsewhere (`ReposJson { repos }`, `AccountListJson { accounts }`) — `crate::json::emit`'s
/// `Envelope` flattens its payload, which requires a struct/map, not a bare array.
#[derive(Serialize)]
struct OnDemandListJson {
    on_demand: Vec<OnDemandListEntry>,
}

/// `cc-loadout profile on-demand list [--json]`.
pub fn cli_list(cwd: &Path, cfg: &Profiles, json: bool) -> Result<()> {
    let state = load_state(cwd)?;
    let rows: Vec<OnDemandListEntry> = cfg
        .on_demand
        .iter()
        .map(|k| OnDemandListEntry {
            key: k.clone(),
            live_holders: state.get(k).map(|e| e.holders.len()).unwrap_or(0),
        })
        .collect();
    if json {
        crate::json::emit(&OnDemandListJson { on_demand: rows })?;
        return Ok(());
    }
    if rows.is_empty() {
        println!("(no on_demand plugins — add one via `profile edit`)");
    }
    for r in rows {
        if r.live_holders > 0 {
            println!("  {} ({} live)", r.key, r.live_holders);
        } else {
            println!("  {}", r.key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_sets_enabled_true_and_records_holder() {
        let dir = tempfile::tempdir().unwrap();
        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(enabled["pixijs@x"], serde_json::json!(true));

        let state = load_state(dir.path()).unwrap();
        assert_eq!(state["pixijs@x"].holders, vec!["sess-1".to_string()]);
        assert_eq!(state["pixijs@x"].prior_value, None);
    }

    #[test]
    fn acquire_snapshots_prior_value_only_on_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, r#"{"enabledPlugins":{"pixijs@x":false}}"#).unwrap();

        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        acquire(dir.path(), "sess-2", "pixijs@x").unwrap();

        let state = load_state(dir.path()).unwrap();
        assert_eq!(state["pixijs@x"].prior_value, Some(false));
        assert_eq!(
            state["pixijs@x"].holders,
            vec!["sess-1".to_string(), "sess-2".to_string()]
        );
    }

    #[test]
    fn release_keeps_plugin_on_while_another_session_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        acquire(dir.path(), "sess-2", "pixijs@x").unwrap();

        release(dir.path(), "sess-1", "pixijs@x", false).unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(
            enabled["pixijs@x"],
            serde_json::json!(true),
            "sess-2 still holds it"
        );
        let state = load_state(dir.path()).unwrap();
        assert_eq!(state["pixijs@x"].holders, vec!["sess-2".to_string()]);
    }

    #[test]
    fn release_last_holder_restores_prior_value_and_drops_entry() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude").join("settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, r#"{"enabledPlugins":{"pixijs@x":false}}"#).unwrap();

        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        release(dir.path(), "sess-1", "pixijs@x", false).unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert_eq!(enabled["pixijs@x"], serde_json::json!(false));
        assert!(!load_state(dir.path()).unwrap().contains_key("pixijs@x"));
    }

    #[test]
    fn release_last_holder_removes_key_when_prior_was_absent() {
        let dir = tempfile::tempdir().unwrap();
        acquire(dir.path(), "sess-1", "pixijs@x").unwrap(); // no prior settings.local.json at all
        release(dir.path(), "sess-1", "pixijs@x", false).unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert!(enabled.get("pixijs@x").is_none());
    }

    #[test]
    fn force_release_reverts_even_with_remaining_holders() {
        let dir = tempfile::tempdir().unwrap();
        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        acquire(dir.path(), "sess-2", "pixijs@x").unwrap();

        release(dir.path(), "sess-1", "pixijs@x", true).unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert!(
            enabled.get("pixijs@x").is_none(),
            "force clears regardless of sess-2"
        );
        assert!(!load_state(dir.path()).unwrap().contains_key("pixijs@x"));
    }

    #[test]
    fn release_unknown_key_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        release(dir.path(), "sess-1", "never-acquired@x", false).unwrap(); // must not error
    }

    #[test]
    fn concurrent_acquire_from_two_sessions_preserves_both_holders() {
        // Regression test for the lost-update race: two sessions acquiring
        // the same key at (nearly) the same time must both end up recorded
        // as holders, never just one. Stress it across several iterations
        // (fresh tempdir each time) since a single race is inherently
        // timing-dependent.
        for _ in 0..30 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let (root1, barrier1) = (root.clone(), barrier.clone());
            let t1 = std::thread::spawn(move || {
                barrier1.wait();
                acquire(&root1, "sess-1", "pixijs@x").unwrap();
            });
            let (root2, barrier2) = (root.clone(), barrier.clone());
            let t2 = std::thread::spawn(move || {
                barrier2.wait();
                acquire(&root2, "sess-2", "pixijs@x").unwrap();
            });
            t1.join().unwrap();
            t2.join().unwrap();

            let state = load_state(&root).unwrap();
            let holders = &state["pixijs@x"].holders;
            assert_eq!(
                holders.len(),
                2,
                "lost update: expected both sess-1 and sess-2 as holders, got {holders:?}"
            );
        }
    }

    #[test]
    fn release_all_releases_every_key_the_session_holds() {
        let dir = tempfile::tempdir().unwrap();
        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        acquire(dir.path(), "sess-1", "coverity@x").unwrap();
        acquire(dir.path(), "sess-2", "coverity@x").unwrap(); // sess-2 also holds coverity

        release_all(dir.path(), "sess-1").unwrap();

        let enabled = crate::profile::apply::current_enabled(dir.path())
            .unwrap()
            .unwrap();
        assert!(
            enabled.get("pixijs@x").is_none(),
            "only sess-1 held pixijs -> fully released"
        );
        assert_eq!(
            enabled["coverity@x"],
            serde_json::json!(true),
            "sess-2 still holds coverity"
        );
    }

    #[test]
    fn cli_acquire_without_session_id_env_var_errors_clearly() {
        // No other test in this file or in main.rs sets/reads
        // CC_LOADOUT_SESSION_ID directly (tests/cli.rs sets it via `.env()` on
        // a spawned subprocess, which is a separate process and cannot race
        // this in-process env mutation), so removing it here is safe against
        // cross-test interference under `cargo test`'s default parallel
        // in-process threading.
        std::env::remove_var("CC_LOADOUT_SESSION_ID");

        let dir = tempfile::tempdir().unwrap();
        let cfg = Profiles {
            on_demand: vec!["pixijs@x".to_string()],
            ..Default::default()
        };

        let err = cli_acquire(dir.path(), &cfg, "pixijs@x").unwrap_err();
        assert!(
            err.to_string().contains("CC_LOADOUT_SESSION_ID"),
            "error must name CC_LOADOUT_SESSION_ID so the user knows what to fix: {err}"
        );
    }

    #[test]
    fn held_keys_lists_only_keys_with_live_holders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(held_keys(dir.path()).unwrap().is_empty());

        acquire(dir.path(), "sess-1", "pixijs@x").unwrap();
        acquire(dir.path(), "sess-1", "gsap@x").unwrap();
        release(dir.path(), "sess-1", "gsap@x", false).unwrap();

        let held = held_keys(dir.path()).unwrap();
        assert!(held.contains("pixijs@x"), "still acquired");
        assert!(!held.contains("gsap@x"), "released -> no live holder");
    }
}
