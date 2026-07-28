//! Account subsystem: snapshot and switch Claude Code login credentials.

pub mod creds;
pub mod cron;
pub mod crontab;
pub mod identity;
pub mod json;
pub mod paths;
pub mod prime;
pub mod store;
pub mod swap;
pub mod timing;

use anyhow::{bail, Context, Result};

use crate::account::paths::ClaudePaths;
use crate::account::store::{AccountMeta, Store};

/// Reject aliases that could escape the data dir (path traversal) or are empty.
/// Allowed: ASCII letters, digits, '.', '_', '-' — but never "." or "..".
pub fn validate_alias(alias: &str) -> Result<()> {
    let ok = !alias.is_empty()
        && alias != "."
        && alias != ".."
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        bail!("invalid alias '{alias}': use only letters, digits, '.', '_', '-' (no '/', no '..')");
    }
    Ok(())
}

/// Snapshot the current live login into a named account slot.
pub fn add(store: &Store, claude: &ClaudePaths, alias: &str, force: bool, now: i64) -> Result<()> {
    validate_alias(alias)?;
    let mut state = store.load_state()?;
    if state.accounts.contains_key(alias) && !force {
        bail!("account '{alias}' already exists (use --force to overwrite)");
    }
    if !claude.credentials.exists() {
        bail!("not logged in: {} not found", claude.credentials.display());
    }
    let creds = creds::read_credentials(&claude.credentials)?;
    let oauth = creds::read_oauth_account(&claude.main_config)?
        .context("no oauthAccount in Claude config (log in with a subscription first)")?;
    let id = identity::extract(&oauth);

    if !force {
        if let Some(existing) = find_duplicate_alias(&state.accounts, &id, alias) {
            let org = id.org_name.as_deref().unwrap_or("personal");
            bail!(
                "this account ({} / {org}) is already saved as '{existing}'; \
                 use --force to add '{alias}' as a duplicate",
                id.email
            );
        }
    }

    store.ensure_account_dir(alias)?;
    creds::write_credentials(&store.credentials_snapshot(alias), &creds)?;
    creds::write_credentials(&store.oauth_snapshot(alias), &oauth)?;

    state.accounts.insert(
        alias.to_string(),
        AccountMeta {
            email: id.email,
            org_uuid: id.org_uuid,
            org_name: id.org_name,
            added_at: now,
            last_used: None,
            last_primed: None,
        },
    );
    if state.active_alias.is_none() {
        state.active_alias = Some(alias.to_string());
    }
    store.save_state(&state)?;
    Ok(())
}

/// Find an existing alias (other than `adding_alias`) that already holds the same
/// account identity (`email` + `org_uuid`) as `id`. Used by `add` to warn before
/// snapshotting the same Claude account twice under different names — keyed by
/// identity, not by alias. An empty `email` is treated as unidentifiable and
/// never matches.
fn find_duplicate_alias(
    accounts: &std::collections::BTreeMap<String, AccountMeta>,
    id: &identity::Identity,
    adding_alias: &str,
) -> Option<String> {
    if id.email.is_empty() {
        return None;
    }
    accounts
        .iter()
        .find(|(alias, meta)| {
            alias.as_str() != adding_alias && meta.email == id.email && meta.org_uuid == id.org_uuid
        })
        .map(|(alias, _)| alias.clone())
}

#[cfg(test)]
mod add_tests {
    use super::*;
    use serde_json::json;

    fn fake_login(home: &std::path::Path, email: &str) -> ClaudePaths {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_vec(&json!({"oauthAccount": {"emailAddress": email}})).unwrap(),
        )
        .unwrap();
        paths::resolve(home, None)
    }

    #[test]
    fn add_snapshots_and_sets_active() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());

        add(&store, &claude, "work", false, 1700000000).unwrap();

        assert!(store.credentials_snapshot("work").exists());
        assert!(store.oauth_snapshot("work").exists());
        let state = store.load_state().unwrap();
        assert_eq!(state.active_alias.as_deref(), Some("work"));
        assert_eq!(state.accounts["work"].email, "a@b.com");
    }

    #[test]
    fn add_existing_without_force_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();
        let err = add(&store, &claude, "work", false, 2).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn add_not_logged_in_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = paths::resolve(hdir.path(), None); // no files created
        let store = Store::new(ddir.path());
        let err = add(&store, &claude, "work", false, 1).unwrap_err();
        assert!(err.to_string().contains("not logged in"));
    }

    #[test]
    fn add_rejects_traversal_alias() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        let err = add(&store, &claude, "../escape", false, 1).unwrap_err();
        assert!(err.to_string().contains("invalid alias"));
        // nothing created outside
        assert!(!ddir.path().join("accounts").join("../escape").exists());
    }

    #[test]
    fn add_creates_account_dir_0700() {
        use std::os::unix::fs::PermissionsExt;
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();
        let mode = std::fs::metadata(store.account_dir("work"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn add_duplicate_account_without_force_errors() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();
        // Same login (same email/org) under a NEW alias must be flagged as a duplicate.
        let err = add(&store, &claude, "work2", false, 2).unwrap_err();
        assert!(
            err.to_string().contains("work"),
            "should name the existing alias: {err}"
        );
        assert!(
            err.to_string().contains("force"),
            "should suggest --force: {err}"
        );
        assert!(!store.load_state().unwrap().accounts.contains_key("work2"));
    }

    #[test]
    fn add_duplicate_account_with_force_succeeds() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();
        // --force allows an intentional duplicate under a different alias.
        add(&store, &claude, "work2", true, 2).unwrap();
        assert_eq!(store.load_state().unwrap().accounts.len(), 2);
    }
}

#[cfg(test)]
mod dedup_tests {
    use super::*;
    use crate::account::identity::Identity;
    use std::collections::BTreeMap;

    fn meta(email: &str, org: Option<&str>) -> AccountMeta {
        AccountMeta {
            email: email.to_string(),
            org_uuid: org.map(str::to_string),
            org_name: None,
            added_at: 0,
            last_used: None,
            last_primed: None,
        }
    }

    fn id(email: &str, org: Option<&str>) -> Identity {
        Identity {
            email: email.to_string(),
            org_uuid: org.map(str::to_string),
            org_name: None,
        }
    }

    #[test]
    fn same_email_and_org_is_duplicate() {
        let mut m = BTreeMap::new();
        m.insert(
            "alice".to_string(),
            meta("alice@example.com", Some("org-1")),
        );
        assert_eq!(
            find_duplicate_alias(&m, &id("alice@example.com", Some("org-1")), "personal")
                .as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn same_email_both_no_org_is_duplicate() {
        let mut m = BTreeMap::new();
        m.insert("work".to_string(), meta("a@b.com", None));
        assert_eq!(
            find_duplicate_alias(&m, &id("a@b.com", None), "work2").as_deref(),
            Some("work")
        );
    }

    #[test]
    fn different_org_is_not_duplicate() {
        // Same person in a different org is a legitimately separate loadout.
        let mut m = BTreeMap::new();
        m.insert(
            "alice".to_string(),
            meta("alice@example.com", Some("org-1")),
        );
        assert!(
            find_duplicate_alias(&m, &id("alice@example.com", Some("org-2")), "personal").is_none()
        );
    }

    #[test]
    fn different_email_is_not_duplicate() {
        let mut m = BTreeMap::new();
        m.insert(
            "alice".to_string(),
            meta("alice@example.com", Some("org-1")),
        );
        assert!(find_duplicate_alias(&m, &id("other@x.com", Some("org-1")), "personal").is_none());
    }

    #[test]
    fn same_alias_is_excluded() {
        // Re-adding the same alias (e.g. `add --force` to freshen) is not self-duplicate.
        let mut m = BTreeMap::new();
        m.insert("work".to_string(), meta("alice@example.com", Some("org-1")));
        assert!(
            find_duplicate_alias(&m, &id("alice@example.com", Some("org-1")), "work").is_none()
        );
    }

    #[test]
    fn empty_email_never_matches() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), meta("", None));
        assert!(find_duplicate_alias(&m, &id("", None), "b").is_none());
    }
}

/// One row for `account list`.
pub struct AccountStatus {
    pub alias: String,
    pub meta: AccountMeta,
    pub is_active: bool,
    /// `claudeAiOauth.expiresAt` (epoch MILLISECONDS). For the ACTIVE account this
    /// comes from the LIVE credentials Claude is actually using (and refreshes in
    /// the background); for the others it comes from the saved snapshot. `None` if
    /// absent/unreadable.
    pub expires_at_ms: Option<i64>,
    /// Whether a non-empty `claudeAiOauth.refreshToken` is present in the same
    /// source as `expires_at_ms`. A present refresh token means the account is
    /// still usable even after the access token's `expiresAt` has passed.
    pub has_refresh: bool,
}

/// Usability of an account's OAuth token, derived for the `account list` display.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TokenStatus {
    /// Access token still valid (`expiresAt` is in the future).
    Ok,
    /// Access token has expired, but a refresh token is present — Claude mints a
    /// fresh access token on next use, so the account is still usable. This is NOT
    /// a problem; it is the normal steady state for an account you haven't switched
    /// to recently.
    Refreshable,
    /// Access token has expired AND no refresh token is present — a real
    /// re-login (`claude` / `cc-loadout account add`) is required.
    Expired,
    /// No expiry information available (snapshot missing or old format).
    Unknown,
}

/// Classify token usability from the raw facts pulled out of a credentials file.
///
/// The display in `account list` must reflect *can I actually use this account*,
/// not merely *has the cached access token's clock run out* — short-lived access
/// tokens expire constantly and are auto-refreshed, so a bare "expired" is
/// misleading whenever a refresh token is on hand.
pub fn classify(expires_at_ms: Option<i64>, has_refresh: bool, now_ms: i64) -> TokenStatus {
    match expires_at_ms {
        // No expiry info at all (snapshot missing or old format) — we can't tell.
        None => TokenStatus::Unknown,
        // Access token still valid.
        Some(exp) if exp > now_ms => TokenStatus::Ok,
        // Expired (`exp <= now_ms`) but refreshable — Claude mints a new token on use.
        Some(_) if has_refresh => TokenStatus::Refreshable,
        // Expired with no way to refresh — a real re-login is required.
        Some(_) => TokenStatus::Expired,
    }
}

/// Format a positive millisecond duration compactly for the `account list` token
/// column: e.g. 11_520_000 -> "3h12m", 2_700_000 -> "45m", 7_200_000 -> "2h",
/// sub-minute -> "<1m". Returns None for a non-positive duration.
pub(crate) fn format_duration_short(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let total_min = ms / 60_000;
    if total_min == 0 {
        return Some("<1m".to_string());
    }
    let (h, m) = (total_min / 60, total_min % 60);
    Some(if h > 0 && m > 0 {
        format!("{h}h{m}m")
    } else if h > 0 {
        format!("{h}h")
    } else {
        format!("{m}m")
    })
}

/// Render the `token:` column value for `account list`: the status word, plus a
/// compact remaining-time hint for a still-valid token (e.g. "ok (3h12m)").
pub fn render_token_status(expires_at_ms: Option<i64>, has_refresh: bool, now_ms: i64) -> String {
    match classify(expires_at_ms, has_refresh, now_ms) {
        TokenStatus::Ok => match expires_at_ms.and_then(|e| format_duration_short(e - now_ms)) {
            Some(rem) => format!("ok ({rem})"),
            None => "ok".to_string(),
        },
        TokenStatus::Refreshable => "refreshable".to_string(),
        TokenStatus::Expired => "expired".to_string(),
        TokenStatus::Unknown => "?".to_string(),
    }
}

/// Pull `(expiresAt_ms, has_non_empty_refresh_token)` out of a credentials JSON
/// file. Missing file / parse error / absent fields degrade to `(None, false)`.
fn read_token_facts(path: &std::path::Path) -> (Option<i64>, bool) {
    match creds::read_credentials(path) {
        Ok(v) => {
            let oauth = v.get("claudeAiOauth");
            let expires_at_ms = oauth
                .and_then(|o| o.get("expiresAt"))
                .and_then(serde_json::Value::as_i64);
            let has_refresh = oauth
                .and_then(|o| o.get("refreshToken"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| !s.is_empty());
            (expires_at_ms, has_refresh)
        }
        Err(_) => (None, false),
    }
}

/// List saved accounts. For the ACTIVE account the token facts are read from the
/// LIVE credentials (`claude.credentials`), since Claude refreshes those in the
/// background and the snapshot would be stale; every other account reads its
/// snapshot.
pub fn list(store: &Store, claude: &ClaudePaths) -> Result<Vec<AccountStatus>> {
    let state = store.load_state()?;
    let active = state.active_alias.clone();
    let mut rows = Vec::new();
    for (alias, meta) in state.accounts {
        let is_active = active.as_deref() == Some(alias.as_str());
        let source = if is_active {
            claude.credentials.clone()
        } else {
            store.credentials_snapshot(&alias)
        };
        let (expires_at_ms, has_refresh) = read_token_facts(&source);
        rows.push(AccountStatus {
            is_active,
            alias,
            meta,
            expires_at_ms,
            has_refresh,
        });
    }
    Ok(rows)
}

/// Return the active alias, if any.
pub fn current(store: &Store) -> Result<Option<String>> {
    Ok(store.load_state()?.active_alias)
}

/// Remove a saved account (its snapshots and its state entry).
pub fn remove(store: &Store, alias: &str) -> Result<()> {
    validate_alias(alias)?;
    let mut state = store.load_state()?;
    if state.accounts.remove(alias).is_none() {
        bail!("unknown account '{alias}'");
    }
    if state.active_alias.as_deref() == Some(alias) {
        state.active_alias = None;
    }
    let dir = store.account_dir(alias);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    store.save_state(&state)?;
    Ok(())
}

#[cfg(test)]
mod list_tests {
    use super::*;
    use serde_json::json;

    fn fake_login(home: &std::path::Path, email: &str) -> ClaudePaths {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_vec(&json!({"oauthAccount": {"emailAddress": email}})).unwrap(),
        )
        .unwrap();
        paths::resolve(home, None)
    }

    #[test]
    fn list_and_current_reflect_active() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();

        let rows = list(&store, &claude).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].is_active);
        assert_eq!(current(&store).unwrap().as_deref(), Some("work"));
    }

    #[test]
    fn remove_deletes_snapshot_and_clears_active() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let claude = fake_login(hdir.path(), "a@b.com");
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();

        remove(&store, "work").unwrap();
        assert!(!store.account_dir("work").exists());
        assert!(current(&store).unwrap().is_none());
        assert!(list(&store, &claude).unwrap().is_empty());
    }

    #[test]
    fn remove_unknown_errors() {
        let ddir = tempfile::tempdir().unwrap();
        let store = Store::new(ddir.path());
        assert!(remove(&store, "nope")
            .unwrap_err()
            .to_string()
            .contains("unknown account"));
    }

    #[test]
    fn list_reports_token_expiry() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(hdir.path().join(".claude")).unwrap();
        std::fs::write(
            hdir.path().join(".claude").join(".credentials.json"),
            br#"{"claudeAiOauth":{"accessToken":"a","expiresAt":1781012734331}}"#,
        )
        .unwrap();
        std::fs::write(
            hdir.path().join(".claude.json"),
            serde_json::to_vec(&json!({"oauthAccount": {"emailAddress": "a@b.com"}})).unwrap(),
        )
        .unwrap();
        let claude = paths::resolve(hdir.path(), None);
        let store = Store::new(ddir.path());
        add(&store, &claude, "work", false, 1).unwrap();

        let rows = list(&store, &claude).unwrap();
        assert_eq!(rows[0].expires_at_ms, Some(1781012734331));
    }

    fn write_live(home: &std::path::Path, creds_json: &str, email: &str) {
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude").join(".credentials.json"), creds_json).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            serde_json::to_vec(&json!({ "oauthAccount": { "emailAddress": email } })).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn list_active_account_reads_live_expiry_not_snapshot() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        let store = Store::new(ddir.path());

        // Snapshot is taken with an already-expired access token.
        write_live(
            home,
            r#"{"claudeAiOauth":{"accessToken":"a","expiresAt":1000,"refreshToken":"r"}}"#,
            "a@b.com",
        );
        let claude = paths::resolve(home, None);
        add(&store, &claude, "work", false, 1).unwrap();

        // Claude refreshes the LIVE token in the background -> new far-future expiry.
        write_live(
            home,
            r#"{"claudeAiOauth":{"accessToken":"a2","expiresAt":9999999999999,"refreshToken":"r2"}}"#,
            "a@b.com",
        );

        let rows = list(&store, &claude).unwrap();
        assert!(rows[0].is_active);
        // The active row must reflect the LIVE token, not the stale snapshot (1000).
        assert_eq!(rows[0].expires_at_ms, Some(9999999999999));
        assert!(rows[0].has_refresh);
    }

    #[test]
    fn list_inactive_account_reads_snapshot_facts() {
        let hdir = tempfile::tempdir().unwrap();
        let ddir = tempfile::tempdir().unwrap();
        let home = hdir.path();
        let store = Store::new(ddir.path());

        // First login = work, snapshotted. `add` sets active only when none is set,
        // so work stays active for the rest of the test.
        write_live(
            home,
            r#"{"claudeAiOauth":{"accessToken":"w","expiresAt":4242,"refreshToken":"wr"}}"#,
            "work@x",
        );
        let claude = paths::resolve(home, None);
        add(&store, &claude, "work", false, 1).unwrap();

        // Second login = personal, snapshotted with expiresAt=5555 (work remains active).
        write_live(
            home,
            r#"{"claudeAiOauth":{"accessToken":"p","expiresAt":5555,"refreshToken":"pr"}}"#,
            "p@x",
        );
        add(&store, &claude, "personal", false, 2).unwrap();

        // Live drifts to a new value; personal (inactive) must ignore it and read
        // its own snapshot (5555), proving inactive accounts do NOT read live.
        write_live(
            home,
            r#"{"claudeAiOauth":{"accessToken":"x","expiresAt":8888,"refreshToken":"xr"}}"#,
            "p@x",
        );

        let rows = list(&store, &claude).unwrap();
        let personal = rows.iter().find(|r| r.alias == "personal").unwrap();
        assert!(!personal.is_active);
        assert_eq!(personal.expires_at_ms, Some(5555));
        assert!(personal.has_refresh);
    }
}

#[cfg(test)]
mod classify_tests {
    use super::{classify, TokenStatus};

    const NOW: i64 = 1_000_000;

    #[test]
    fn valid_access_token_is_ok() {
        assert_eq!(classify(Some(NOW + 60_000), true, NOW), TokenStatus::Ok);
    }

    #[test]
    fn valid_access_token_is_ok_even_without_refresh() {
        assert_eq!(classify(Some(NOW + 60_000), false, NOW), TokenStatus::Ok);
    }

    #[test]
    fn expired_with_refresh_token_is_refreshable() {
        assert_eq!(classify(Some(NOW - 1), true, NOW), TokenStatus::Refreshable);
    }

    #[test]
    fn expired_without_refresh_token_is_expired() {
        assert_eq!(classify(Some(NOW - 1), false, NOW), TokenStatus::Expired);
    }

    #[test]
    fn boundary_exactly_now_counts_as_expired() {
        // `expiresAt == now` is treated as expired (matches the original `<= now`).
        assert_eq!(classify(Some(NOW), true, NOW), TokenStatus::Refreshable);
        assert_eq!(classify(Some(NOW), false, NOW), TokenStatus::Expired);
    }

    #[test]
    fn missing_expiry_is_unknown() {
        assert_eq!(classify(None, true, NOW), TokenStatus::Unknown);
        assert_eq!(classify(None, false, NOW), TokenStatus::Unknown);
    }
}

#[cfg(test)]
mod render_tests {
    use super::{format_duration_short, render_token_status};

    const NOW: i64 = 1_000_000_000_000;
    const H: i64 = 3_600_000;
    const M: i64 = 60_000;

    #[test]
    fn format_hours_and_minutes() {
        assert_eq!(
            format_duration_short(3 * H + 12 * M).as_deref(),
            Some("3h12m")
        );
    }

    #[test]
    fn format_minutes_only() {
        assert_eq!(format_duration_short(45 * M).as_deref(), Some("45m"));
    }

    #[test]
    fn format_whole_hours_drops_minutes() {
        assert_eq!(format_duration_short(2 * H).as_deref(), Some("2h"));
    }

    #[test]
    fn format_sub_minute_is_lt1m() {
        assert_eq!(format_duration_short(30_000).as_deref(), Some("<1m"));
    }

    #[test]
    fn format_non_positive_is_none() {
        assert_eq!(format_duration_short(0), None);
        assert_eq!(format_duration_short(-5), None);
    }

    #[test]
    fn render_ok_includes_remaining_time() {
        assert_eq!(render_token_status(Some(NOW + 3 * H), true, NOW), "ok (3h)");
    }

    #[test]
    fn render_other_states_have_no_time() {
        assert_eq!(render_token_status(Some(NOW - 1), true, NOW), "refreshable");
        assert_eq!(render_token_status(Some(NOW - 1), false, NOW), "expired");
        assert_eq!(render_token_status(None, true, NOW), "?");
    }
}
