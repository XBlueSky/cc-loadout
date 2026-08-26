use anyhow::{bail, Context, Result};
use std::path::Path;
use std::time::Duration;

use crate::account::prime::wait_with_timeout;

/// Everything one headless `claude -p` invocation needs. A struct rather than a
/// parameter list because the list had already reached the point where callers
/// could silently swap two same-typed arguments.
pub struct RunSpec<'a> {
    /// `Some` ⇒ run isolated under this CLAUDE_CONFIG_DIR; `None` ⇒ inherit the
    /// live ~/.claude.
    pub cfg_dir: Option<&'a Path>,
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub session_id: &'a str,
    /// `Some` ⇒ pass `--model`; `None` ⇒ pass no flag and let the CLI resolve
    /// its own default (settings.json / account default).
    pub model: Option<&'a str>,
    pub timeout: Duration,
    /// Exports `CORTEX_SKIP_RECORD=1` to the child, telling cortex's SessionEnd
    /// hook not to record the session into the vault's Raw/ — used for probe
    /// pings whose transcripts carry no distill-worthy content. Real tasks pass
    /// `false`, which instead exports `CORTEX_FORCE_RECORD=1`: cortex now drops
    /// headless (`entrypoint: sdk-cli`) sessions by default to keep stray eval
    /// harnesses out of the vault, and a scheduled task is the one headless
    /// caller whose work IS worth recording.
    pub skip_record: bool,
}

/// Run `claude -p <prompt> --session-id <id> [--model <m>]` in `spec.cwd`.
/// Kills the child on timeout.
pub fn run_claude(claude_bin: &Path, spec: &RunSpec<'_>) -> Result<()> {
    let mut child = crate::util::retry_etxtbsy(|| {
        let mut cmd = std::process::Command::new(claude_bin);
        cmd.arg("-p")
            .arg(spec.prompt)
            .arg("--session-id")
            .arg(spec.session_id)
            .current_dir(spec.cwd)
            .stdin(std::process::Stdio::null());
        if let Some(model) = spec.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(dir) = spec.cfg_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        if spec.skip_record {
            cmd.env("CORTEX_SKIP_RECORD", "1");
        } else {
            cmd.env("CORTEX_FORCE_RECORD", "1");
        }
        cmd.spawn()
    })
    .context("running `claude -p`")?;

    let status = wait_with_timeout(&mut child, spec.timeout)?;
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

    /// A spec with the given model, pointed at `cwd`; other fields are defaults
    /// the model tests don't care about.
    fn spec<'a>(cwd: &'a Path, model: Option<&'a str>) -> RunSpec<'a> {
        RunSpec {
            cfg_dir: None,
            cwd,
            prompt: "hi",
            session_id: "55555555-5555-5555-5555-555555555555",
            model,
            timeout: Duration::from_secs(10),
            skip_record: false,
        }
    }

    /// Fake claude that dumps its whole argv, one word per line, into `argv-dump`.
    fn fake_argv_dump_claude(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\n: > argv-dump\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> argv-dump; done\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn a_model_is_passed_through_as_a_model_flag() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_argv_dump_claude(bin.path());
        run_claude(&claude, &spec(cwd.path(), Some("haiku"))).unwrap();
        let argv = std::fs::read_to_string(cwd.path().join("argv-dump")).unwrap();
        let words: Vec<&str> = argv.lines().collect();
        let i = words
            .iter()
            .position(|w| *w == "--model")
            .expect("--model must be passed");
        assert_eq!(words.get(i + 1), Some(&"haiku"));
    }

    #[test]
    fn no_model_leaves_the_flag_off_so_the_cli_picks() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_argv_dump_claude(bin.path());
        run_claude(&claude, &spec(cwd.path(), None)).unwrap();
        let argv = std::fs::read_to_string(cwd.path().join("argv-dump")).unwrap();
        assert!(
            !argv.lines().any(|w| w == "--model"),
            "no model must mean no --model flag, not an empty one: {argv:?}"
        );
    }

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

    /// Fake claude that dumps `${CORTEX_SKIP_RECORD:-}` into `env-dump` and
    /// `${CORTEX_FORCE_RECORD:-}` into `force-dump`, both in cwd.
    fn fake_env_dump_claude(dir: &Path) -> std::path::PathBuf {
        let p = dir.join("claude");
        std::fs::write(
            &p,
            "#!/bin/sh\nprintf '%s' \"${CORTEX_SKIP_RECORD:-}\" > env-dump\n\
             printf '%s' \"${CORTEX_FORCE_RECORD:-}\" > force-dump\nexit 0\n",
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
            &RunSpec {
                prompt: "ping",
                session_id: "33333333-3333-3333-3333-333333333333",
                skip_record: true, // probe-style run
                ..spec(cwd.path(), None)
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("env-dump")).unwrap(),
            "1",
            "skip_record=true must export CORTEX_SKIP_RECORD=1 to the child"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("force-dump")).unwrap(),
            "",
            "skip_record=true must not also force recording"
        );
    }

    #[test]
    fn normal_run_leaves_cortex_skip_record_unset() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_env_dump_claude(bin.path());
        run_claude(
            &claude,
            &RunSpec {
                prompt: "real work",
                session_id: "44444444-4444-4444-4444-444444444444",
                skip_record: false, // real task, keep cortex recording
                ..spec(cwd.path(), None)
            },
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("env-dump")).unwrap(),
            "",
            "skip_record=false must not export CORTEX_SKIP_RECORD"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.path().join("force-dump")).unwrap(),
            "1",
            "skip_record=false must export CORTEX_FORCE_RECORD=1 so cortex's \
             headless guard does not drop a real scheduled task"
        );
    }

    #[test]
    fn runs_claude_with_session_id_in_cwd() {
        let bin = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let claude = fake_claude(bin.path());
        run_claude(
            &claude,
            &RunSpec {
                prompt: "hello",
                session_id: "11111111-1111-1111-1111-111111111111",
                ..spec(cwd.path(), None)
            },
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
            &RunSpec {
                prompt: "x",
                session_id: "22222222-2222-2222-2222-222222222222",
                timeout: Duration::from_secs(5),
                ..spec(cwd.path(), None)
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("claude -p"));
    }
}
