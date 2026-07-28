use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Resolve the `crontab` binary to an absolute path on `$PATH`.
///
/// Callers previously passed a bare `Path::new("crontab")`, which relies on the
/// spawning process's `$PATH` at the moment of the write. When cc-loadout is launched
/// from an environment whose `$PATH` lacks the system bindir (some launchers, service
/// managers, or a sandbox that shims `crontab`), the write silently goes nowhere — the
/// schedule is saved but never installed into cron. Resolving up front turns that
/// into a clear, actionable error instead.
pub fn resolve_bin() -> Result<PathBuf> {
    // Test safety net: a unit test must NEVER resolve the developer's real `crontab`
    // and let a background job (e.g. a TUI remove/write-schedule job) splice their live
    // user crontab — that race once wiped a real prime schedule. This branch compiles
    // only under `cargo test` for THIS crate; unit tests exercise crontab logic with an
    // injected fake bin instead. The integration binary (cfg not test) is isolated via
    // PATH by `tests/cli.rs::cmd()`, and production is unaffected.
    #[cfg(test)]
    {
        bail!(
            "resolve_bin() is disabled in unit-test builds so no test can touch the real \
             user crontab; pass an explicit fake `crontab` bin to the crontab ops instead"
        );
    }
    #[cfg(not(test))]
    crate::util::which("crontab").ok_or_else(|| {
        anyhow::anyhow!(
            "`crontab` not found on $PATH — cannot install the schedule into cron. \
             Install cron and ensure its bindir (e.g. /usr/bin) is on PATH."
        )
    })
}

/// Read the current crontab text using an explicit `crontab` binary path.
/// The real `crontab -l` exits non-zero when no table exists; treat that as
/// an empty table.
pub(crate) fn read_with(crontab_bin: &Path) -> Result<String> {
    let out = crate::util::retry_etxtbsy(|| Command::new(crontab_bin).arg("-l").output())
        .context("running `crontab -l`")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Ok(String::new())
    }
}

/// Replace the crontab with `text` via an explicit `crontab` binary.
pub(crate) fn write_with(crontab_bin: &Path, text: &str) -> Result<()> {
    let mut child = crate::util::retry_etxtbsy(|| {
        Command::new(crontab_bin)
            .arg("-")
            .stdin(Stdio::piped())
            .spawn()
    })
    .context("spawning `crontab -`")?;
    child
        .stdin
        .take()
        .context("crontab stdin unavailable")?
        .write_all(text.as_bytes())
        .context("writing crontab")?;
    let status = child
        .wait()
        .with_context(|| format!("waiting for `{} -`", crontab_bin.display()))?;
    if !status.success() {
        bail!(
            "`{} -` failed (exit {:?})",
            crontab_bin.display(),
            status.code()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The test safety net: no unit test may resolve the real system `crontab`, so a
    /// background job spawned during a test can never splice the developer's live user
    /// crontab. Regression for the wipe where a TUI remove-job did exactly that.
    #[test]
    fn resolve_bin_refuses_in_unit_test_builds() {
        assert!(super::resolve_bin().is_err());
    }
}
