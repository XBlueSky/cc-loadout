use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::account::paths;
use crate::account::store::Store;
use crate::account::{creds, validate_alias};
use crate::util;

/// Max wall-clock seconds to wait for a prime's `claude -p` before killing it.
/// Bounds how long prime can hold the data-dir lock if claude hangs.
const PRIME_TIMEOUT_SECS: u64 = 120;

/// Wait for `child` up to `timeout`, killing it (and reaping) on timeout.
pub(crate) fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("polling `claude -p`")? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("`claude -p` timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Outcome of a prime attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum PrimeOutcome {
    /// A `claude -p` request was fired as `alias`, anchoring its window.
    Primed,
    /// `alias` is the active foreground account — skipped to avoid rotating the
    /// live session's token out from under it. Its window opens on normal use.
    SkippedActive,
}

/// Whether the prime claude binary is runnable: an explicit path must be a file;
/// the bare default `claude` is looked up on PATH.
fn claude_runnable(claude_bin: &std::path::Path) -> bool {
    if claude_bin == std::path::Path::new("claude") {
        crate::util::claude_on_path()
    } else {
        claude_bin.is_file()
    }
}

/// Fire a minimal `claude -p` as `alias` to anchor its 5-hour window, without
/// disturbing the active foreground account. Materialises an ephemeral
/// `CLAUDE_CONFIG_DIR` from the alias's snapshot, runs `claude`, then syncs the
/// (possibly token-rotated) credentials back to the snapshot so a later
/// `account use <alias>` restores a live token. Skips the active account.
pub fn prime(
    store: &Store,
    claude_live: &paths::ClaudePaths,
    home: &Path,
    alias: &str,
    prompt: &str,
    now: i64,
) -> Result<PrimeOutcome> {
    prime_with(
        std::path::Path::new("claude"),
        store,
        claude_live,
        home,
        alias,
        prompt,
        now,
    )
}

pub(crate) fn prime_with(
    claude_bin: &Path,
    store: &Store,
    _claude_live: &paths::ClaudePaths,
    home: &Path,
    alias: &str,
    prompt: &str,
    now: i64,
) -> Result<PrimeOutcome> {
    validate_alias(alias)?;
    let _lock = util::lock::acquire(&store.lock_path())?;
    let mut state = store.load_state()?;
    if !state.accounts.contains_key(alias) {
        bail!("unknown account '{alias}'");
    }
    if state.active_alias.as_deref() == Some(alias) {
        return Ok(PrimeOutcome::SkippedActive);
    }
    if !claude_runnable(claude_bin) {
        bail!("`claude` not found — cannot prime '{alias}'");
    }

    let snap_creds = store.credentials_snapshot(alias);
    let snap_oauth = store.oauth_snapshot(alias);
    if !snap_creds.exists() {
        bail!(
            "no credential snapshot for '{alias}' at {}",
            snap_creds.display()
        );
    }

    // 1. Materialise an ephemeral, isolated config dir from the snapshot.
    let tmp = tempfile::tempdir().context("creating ephemeral config dir")?;
    let cfg_dir = tmp.path().join("cfg");
    std::fs::create_dir_all(&cfg_dir).context("creating ephemeral config dir")?;
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o700);
        let _ = std::fs::set_permissions(tmp.path(), perm.clone());
        let _ = std::fs::set_permissions(&cfg_dir, perm);
    }
    let eph = paths::resolve(home, Some(&cfg_dir));
    let creds_val = creds::read_credentials(&snap_creds)?;
    creds::write_credentials(&eph.credentials, &creds_val)?;
    if let Ok(oauth) = creds::read_credentials(&snap_oauth) {
        // Real `claude` under CLAUDE_CONFIG_DIR may read oauthAccount from the
        // sibling `<dir>.json` (paths::resolve's model) OR the inner
        // `<dir>/.claude.json`. Write both so identity lands regardless —
        // pending real-claude confirmation at acceptance.
        creds::write_oauth_account(&eph.main_config, &oauth)?;
        creds::write_oauth_account(&cfg_dir.join(".claude.json"), &oauth)?;
    }

    // 2. Run `claude -p <prompt>` isolated to that config dir.
    // CORTEX_SKIP_RECORD: probe pings carry no distill-worthy content — tell
    // cortex's SessionEnd hook (inherits this env) not to record a Raw.
    let mut child = crate::util::retry_etxtbsy(|| {
        std::process::Command::new(claude_bin)
            .arg("-p")
            .arg(prompt)
            .env("CLAUDE_CONFIG_DIR", &cfg_dir)
            .env("CORTEX_SKIP_RECORD", "1")
            .stdin(std::process::Stdio::null())
            .spawn()
    })
    .context("running `claude -p` to prime the window")?;

    let status = wait_with_timeout(
        &mut child,
        std::time::Duration::from_secs(PRIME_TIMEOUT_SECS),
    )?;
    if !status.success() {
        bail!(
            "`claude -p` failed for '{alias}' (exit {:?})",
            status.code()
        );
    }

    // 3. Sync the (possibly rotated) credentials back to the snapshot.
    match creds::read_credentials(&eph.credentials) {
        Ok(rotated) if rotated != creds_val => creds::write_credentials(&snap_creds, &rotated)?,
        Ok(_) => {}
        Err(e) => eprintln!(
            "warning: prime '{alias}': could not parse refreshed credentials; \
             snapshot left unchanged: {e:#}"
        ),
    }

    // 4. Record the prime.
    if let Some(meta) = state.accounts.get_mut(alias) {
        meta.last_primed = Some(now);
    }
    store.save_state(&state)?;
    Ok(PrimeOutcome::Primed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Install a fake `claude` on PATH that (1) asserts it is run isolated,
    /// (2) simulates token ROTATION by rewriting `$CLAUDE_CONFIG_DIR/.credentials.json`.
    fn fake_claude(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             test -n \"$CLAUDE_CONFIG_DIR\" || { echo no-config-dir >&2; exit 3; }\n\
             printf '%s' '{\"claudeAiOauth\":{\"accessToken\":\"ROTATED\",\"refreshToken\":\"NEWREFRESH\"}}' \
               > \"$CLAUDE_CONFIG_DIR/.credentials.json\"\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn snapshot_account(store: &Store, alias: &str) {
        store.ensure_account_dir(alias).unwrap();
        creds::write_credentials(
            &store.credentials_snapshot(alias),
            &json!({"claudeAiOauth": {"accessToken": "OLD", "refreshToken": "OLDREFRESH"}}),
        )
        .unwrap();
        creds::write_credentials(
            &store.oauth_snapshot(alias),
            &json!({"emailAddress": "a@b.com"}),
        )
        .unwrap();
    }

    #[test]
    fn prime_runs_claude_and_syncs_rotated_token_back_to_snapshot() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        fake_claude(bin.path());

        let store = Store::new(data.path());
        snapshot_account(&store, "work");
        let mut st = store.load_state().unwrap();
        st.accounts.insert("work".into(), Default::default());
        st.accounts.insert("other".into(), Default::default());
        st.active_alias = Some("other".into());
        store.save_state(&st).unwrap();

        let claude_live = paths::resolve(home.path(), None);
        let out = prime_with(
            &bin.path().join("claude"),
            &store,
            &claude_live,
            home.path(),
            "work",
            "ok",
            1700,
        )
        .unwrap();
        assert_eq!(out, PrimeOutcome::Primed);

        let snap = creds::read_credentials(&store.credentials_snapshot("work")).unwrap();
        assert_eq!(snap["claudeAiOauth"]["refreshToken"], json!("NEWREFRESH"));
        assert_eq!(
            store.load_state().unwrap().accounts["work"].last_primed,
            Some(1700)
        );
    }

    #[test]
    fn prime_marks_probe_session_skip_record_for_cortex() {
        // Probe pings exist only to tick the 5h usage window — they carry no
        // distill-worthy content. CORTEX_SKIP_RECORD=1 tells cortex's
        // SessionEnd hook (which inherits the child's env) not to write a
        // junk Raw. The fake claude fails hard when the marker is missing.
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let script = bin.path().join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             test \"$CORTEX_SKIP_RECORD\" = 1 || { echo not-marked >&2; exit 5; }\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let store = Store::new(data.path());
        snapshot_account(&store, "work");
        let mut st = store.load_state().unwrap();
        st.accounts.insert("work".into(), Default::default());
        st.active_alias = Some("other".into());
        store.save_state(&st).unwrap();

        let claude_live = paths::resolve(home.path(), None);
        let out = prime_with(
            &script,
            &store,
            &claude_live,
            home.path(),
            "work",
            "ok",
            1700,
        )
        .expect("prime must set CORTEX_SKIP_RECORD=1 on its claude -p");
        assert_eq!(out, PrimeOutcome::Primed);
    }

    #[test]
    fn prime_skips_the_active_account() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        snapshot_account(&store, "work");
        let mut st = store.load_state().unwrap();
        st.accounts.insert("work".into(), Default::default());
        st.active_alias = Some("work".into());
        store.save_state(&st).unwrap();

        let claude_live = paths::resolve(home.path(), None);
        let out = prime(&store, &claude_live, home.path(), "work", "ok", 1700).unwrap();
        assert_eq!(out, PrimeOutcome::SkippedActive);
        let snap = creds::read_credentials(&store.credentials_snapshot("work")).unwrap();
        assert_eq!(snap["claudeAiOauth"]["refreshToken"], json!("OLDREFRESH"));
    }

    #[test]
    fn prime_unknown_alias_errors() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let claude_live = paths::resolve(home.path(), None);
        assert!(prime(&store, &claude_live, home.path(), "ghost", "ok", 1)
            .unwrap_err()
            .to_string()
            .contains("unknown account"));
    }
}
