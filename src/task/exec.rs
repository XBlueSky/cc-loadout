use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Duration;

use crate::account::prime::wait_with_timeout;

/// Run `claude -p <prompt> --session-id <session_id>` in `cwd`. When `cfg_dir`
/// is `Some`, run isolated under that CLAUDE_CONFIG_DIR; when `None`, inherit the
/// live ~/.claude. Kills the child on timeout.
///
/// `skip_record` exports `CORTEX_SKIP_RECORD=1` to the child, telling cortex's
/// SessionEnd hook not to record the session into the vault's Raw/ — used for
/// probe pings whose transcripts carry no distill-worthy content. Real tasks
/// pass `false` so their sessions stay recorded.
pub fn run_claude(
    claude_bin: &Path,
    cfg_dir: Option<&Path>,
    cwd: &Path,
    prompt: &str,
    session_id: &str,
    timeout: Duration,
    skip_record: bool,
) -> Result<()> {
    let mut child = crate::util::retry_etxtbsy(|| {
        let mut cmd = std::process::Command::new(claude_bin);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--session-id")
            .arg(session_id)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null());
        if let Some(dir) = cfg_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        if skip_record {
            cmd.env("CORTEX_SKIP_RECORD", "1");
        }
        cmd.spawn()
    })
    .context("running `claude -p`")?;

    let status = wait_with_timeout(&mut child, timeout)?;
    if !status.success() {
        bail!("`claude -p` failed (exit {:?})", status.code());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    /// Fake claude: asserts CLAUDE_CONFIG_DIR + --session-id were passed, then
    /// writes a marker file named after the session id into the cwd.
    fn fake_claude(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\n\
             sid=\"\"\n\
             while [ $# -gt 0 ]; do case \"$1\" in --session-id) shift; sid=\"$1\";; esac; shift; done\n\
             test -n \"$sid\" || exit 4\n\
             : > \"ran-$sid\"\n\
             exit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Fake claude that dumps `${CORTEX_SKIP_RECORD:-}` into `env-dump` in cwd.
    fn fake_env_dump_claude(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\nprintf '%s' \"${CORTEX_SKIP_RECORD:-}\" > env-dump\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn skip_record_run_exports_cortex_skip_record() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_env_dump_claude(bin.path());
        run_claude(
            &claude,
            None,
            cwd.path(),
            "ping",
            "33333333-3333-3333-3333-333333333333",
            Duration::from_secs(10),
            true, // skip_record: probe-style run
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("env-dump")).unwrap(),
            "1",
            "skip_record=true must export CORTEX_SKIP_RECORD=1 to the child"
        );
    }

    #[test]
    fn normal_run_leaves_cortex_skip_record_unset() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_env_dump_claude(bin.path());
        run_claude(
            &claude,
            None,
            cwd.path(),
            "real work",
            "44444444-4444-4444-4444-444444444444",
            Duration::from_secs(10),
            false, // skip_record: real task, keep cortex recording
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("env-dump")).unwrap(),
            "",
            "skip_record=false must not export CORTEX_SKIP_RECORD"
        );
    }

    #[test]
    fn runs_claude_with_session_id_in_cwd() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_claude(bin.path());
        run_claude(
            &claude,
            None,
            cwd.path(),
            "hello",
            "11111111-1111-1111-1111-111111111111",
            Duration::from_secs(10),
            false,
        )
        .unwrap();
        assert!(cwd
            .path()
            .join("ran-11111111-1111-1111-1111-111111111111")
            .exists());
    }

    #[test]
    fn nonzero_exit_is_an_error() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let p = bin.path().join("claude");
        std::fs::write(&p, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = run_claude(
            &p,
            None,
            cwd.path(),
            "x",
            "22222222-2222-2222-2222-222222222222",
            Duration::from_secs(5),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("claude -p"));
    }
}
