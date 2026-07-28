use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::account::paths::{self, ClaudePaths};
use crate::account::{creds, store::Store};

/// Read back the live `oauthAccount` and confirm it matches `expected_email`.
/// Catches the case where the write did not stick — e.g. Claude was running and
/// rewrote the config, a partial/raced write, or (defensively) a config-layout
/// change where our write landed somewhere Claude does not read.
fn verify_live_account(claude: &ClaudePaths, expected_email: &str) -> Result<()> {
    let live_email = creds::read_oauth_account(&claude.main_config)
        .ok()
        .flatten()
        .map(|v| crate::account::identity::extract(&v).email)
        .unwrap_or_default();
    if live_email != expected_email {
        bail!(
            "post-switch verification failed: live account is '{live_email}', expected '{expected_email}'. \
             The switch did not take effect (Claude may have been running and overwrote the config, \
             or Claude's config layout changed). The previous login has been restored — quit Claude and retry."
        );
    }
    Ok(())
}

/// Result of a successful switch.
#[derive(Debug)]
pub struct SwitchOutcome {
    /// Previous active alias (reserved for future logging / undo).
    #[allow(dead_code)]
    pub from: Option<String>,
    pub to: String,
    /// Non-fatal warning (e.g. stale inner config file).
    pub warning: Option<String>,
}

/// Switch the live login to `alias`, transactionally.
///
/// 1. lock
/// 2. read TARGET snapshot first (abort cleanly if missing/corrupt — never touch live)
/// 3. refresh-back CURRENT live into the current alias's snapshot
/// 4. write TARGET to live (credentials atomically, then surgical oauthAccount;
///    roll credentials back if the oauthAccount write fails)
/// 5. update state.json
pub fn switch(
    store: &Store,
    claude: &ClaudePaths,
    home: &Path,
    alias: &str,
    now: i64,
) -> Result<SwitchOutcome> {
    crate::account::validate_alias(alias)?;
    let _lock = crate::util::lock::acquire(&store.lock_path())?;

    let mut state = store.load_state()?;
    if !state.accounts.contains_key(alias) {
        bail!("unknown account '{alias}'");
    }

    // Stage 1: read target snapshot BEFORE touching anything live.
    let target_creds = creds::read_credentials(&store.credentials_snapshot(alias))
        .with_context(|| format!("reading credentials snapshot for '{alias}'"))?;
    let target_oauth = creds::read_credentials(&store.oauth_snapshot(alias))
        .with_context(|| format!("reading oauth snapshot for '{alias}'"))?;
    let target_email = crate::account::identity::extract(&target_oauth).email;

    // Stage 2: refresh-back the current live state into the current alias snapshot,
    // so token refreshes since the last `add` are not lost. Best-effort.
    let from = state.active_alias.clone();
    if let Some(cur) = &from {
        if claude.credentials.exists() {
            if let Ok(live) = creds::read_credentials(&claude.credentials) {
                let _ = creds::write_credentials(&store.credentials_snapshot(cur), &live);
            }
        }
        if let Ok(Some(live_oauth)) = creds::read_oauth_account(&claude.main_config) {
            let _ = creds::write_credentials(&store.oauth_snapshot(cur), &live_oauth);
        }
    }

    let warning = paths::stale_inner_warning(home, &claude.main_config);

    // Stage 3: write target to live, rolling credentials back if oauth write fails.
    let prev_live = if claude.credentials.exists() {
        creds::read_credentials(&claude.credentials).ok()
    } else {
        None
    };
    let prev_oauth = creds::read_oauth_account(&claude.main_config)
        .ok()
        .flatten();
    creds::write_credentials(&claude.credentials, &target_creds)?;
    if let Err(e) = creds::write_oauth_account(&claude.main_config, &target_oauth) {
        if let Some(prev) = prev_live {
            let _ = creds::write_credentials(&claude.credentials, &prev);
        }
        return Err(e.context("writing oauthAccount; rolled back credentials"));
    }

    // Stage 3.5: read-back verification — confirm the switch actually took effect.
    if let Err(e) = verify_live_account(claude, &target_email) {
        // Restore the previous login best-effort, then fail loudly. State is NOT saved.
        if let Some(prev) = &prev_live {
            let _ = creds::write_credentials(&claude.credentials, prev);
        }
        if let Some(prev) = &prev_oauth {
            let _ = creds::write_oauth_account(&claude.main_config, prev);
        }
        return Err(e);
    }

    // Stage 4: commit state.
    if let Some(meta) = state.accounts.get_mut(alias) {
        meta.last_used = Some(now);
    }
    state.active_alias = Some(alias.to_string());
    store.save_state(&state)?;

    Ok(SwitchOutcome {
        from,
        to: alias.to_string(),
        warning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{add, paths, store::Store};
    use serde_json::json;

    fn write_live(home: &std::path::Path, email: &str) {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            format!(r#"{{"claudeAiOauth":{{"accessToken":"tok-{email}"}}}}"#),
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_vec(
                &json!({"alpha": 1, "oauthAccount": {"emailAddress": email}, "zeta": 2}),
            )
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn switch_swaps_credentials_and_oauth() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        let claude = paths::resolve(home, None);
        let store = Store::new(ddir.path());

        write_live(home, "a@b.com");
        add(&store, &claude, "work", false, 1).unwrap();
        write_live(home, "p@e.com");
        add(&store, &claude, "personal", false, 2).unwrap();

        let outcome = switch(&store, &claude, home, "work", 3).unwrap();
        assert_eq!(outcome.to, "work");

        let creds = std::fs::read_to_string(&claude.credentials).unwrap();
        assert!(creds.contains("tok-a@b.com"));
        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&claude.main_config).unwrap()).unwrap();
        assert_eq!(cfg["oauthAccount"]["emailAddress"], json!("a@b.com"));
        assert_eq!(cfg["alpha"], json!(1));
        assert_eq!(cfg["zeta"], json!(2));

        assert_eq!(
            store.load_state().unwrap().active_alias.as_deref(),
            Some("work")
        );
    }

    #[test]
    fn switch_unknown_alias_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = paths::resolve(hdir.path(), None);
        let store = Store::new(ddir.path());
        let err = switch(&store, &claude, hdir.path(), "nope", 1).unwrap_err();
        assert!(err.to_string().contains("unknown account"));
    }

    #[test]
    fn switch_rejects_traversal_alias() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = paths::resolve(hdir.path(), None);
        let store = Store::new(ddir.path());
        let err = switch(&store, &claude, hdir.path(), "../x", 1).unwrap_err();
        assert!(err.to_string().contains("invalid alias"));
    }

    #[test]
    fn switch_missing_snapshot_leaves_live_untouched() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        let claude = paths::resolve(home, None);
        let store = Store::new(ddir.path());

        write_live(home, "a@b.com");
        add(&store, &claude, "work", false, 1).unwrap();
        write_live(home, "p@e.com");
        add(&store, &claude, "personal", false, 2).unwrap();
        std::fs::remove_file(store.credentials_snapshot("work")).unwrap();

        let before = std::fs::read_to_string(&claude.credentials).unwrap();
        let err = switch(&store, &claude, home, "work", 3).unwrap_err();
        assert!(err.to_string().contains("snapshot"));
        let after = std::fs::read_to_string(&claude.credentials).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn verify_live_account_matches() {
        let hdir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"a@b.com"}}"#,
        )
        .unwrap();
        let claude = paths::resolve(home, None);
        assert!(verify_live_account(&claude, "a@b.com").is_ok());
    }

    #[test]
    fn verify_live_account_mismatch_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"other@x"}}"#,
        )
        .unwrap();
        let claude = paths::resolve(home, None);
        let err = verify_live_account(&claude, "a@b.com").unwrap_err();
        assert!(err.to_string().contains("verification failed"));
    }

    #[test]
    fn verify_live_account_missing_oauth_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        std::fs::write(home.join(".claude.json"), br#"{"numStartups":1}"#).unwrap();
        let claude = paths::resolve(home, None);
        // missing oauthAccount → live_email == "" != expected → error
        assert!(verify_live_account(&claude, "a@b.com").is_err());
    }

    #[test]
    fn switch_rolls_back_credentials_on_oauth_write_failure() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        let claude = paths::resolve(home, None);
        let store = Store::new(ddir.path());

        // snapshot 'work' (a@b.com), then make live = personal (p@e.com)
        write_live(home, "a@b.com");
        add(&store, &claude, "work", false, 1).unwrap();
        write_live(home, "p@e.com");
        add(&store, &claude, "personal", false, 2).unwrap();

        // Sabotage: replace live .claude.json with a directory so write_oauth_account fails.
        std::fs::remove_file(&claude.main_config).unwrap();
        std::fs::create_dir(&claude.main_config).unwrap();

        // live creds currently belong to personal (tok-p@e.com)
        let before = std::fs::read_to_string(&claude.credentials).unwrap();
        assert!(before.contains("tok-p@e.com"));

        // switching to 'work' writes tok-a@b.com, then oauth write fails -> rollback to tok-p@e.com
        let err = switch(&store, &claude, home, "work", 3).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("oauth")
                || err.to_string().contains("rolled back")
        );

        // The rollback writes via write_credentials (pretty-printed), so byte-for-byte
        // equality with the original compact JSON is not guaranteed.  What matters is
        // that the token is restored to the pre-switch value.
        let after = std::fs::read_to_string(&claude.credentials).unwrap();
        assert!(
            after.contains("tok-p@e.com"),
            "credentials must be rolled back to personal token"
        );
        assert!(
            !after.contains("tok-a@b.com"),
            "work token must not remain after rollback"
        );
    }
}
