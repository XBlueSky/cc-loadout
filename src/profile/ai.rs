use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::profile::config::Profiles;
use crate::profile::discover::Inventory;
use crate::profile::init::{assemble_profiles, validate, Assignment};

pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";

pub const ASSIGNMENT_SCHEMA: &str = r#"{"type":"object","properties":{"universal":{"type":"array","items":{"type":"string"}},"profiles":{"type":"object","additionalProperties":{"type":"array","items":{"type":"string"}}}},"required":["universal","profiles"],"additionalProperties":false}"#;

/// Build the user prompt: the installed plugins + the scan-suggested profiles
/// (with their detect markers), asking for a plugin→profile assignment.
pub fn build_prompt(inv: &Inventory) -> String {
    let plugins: Vec<&str> = inv.plugins.iter().map(|p| p.key.as_str()).collect();
    let mut profiles = String::new();
    for s in &inv.suggested_profiles {
        let markers: Vec<String> = s
            .shared_signals
            .marker_files
            .iter()
            .chain(s.shared_signals.marker_globs.iter())
            .cloned()
            .collect();
        profiles.push_str(&format!(
            "  - {} (repos with: {})\n",
            s.name,
            markers.join(", ")
        ));
    }
    format!(
        "You are configuring Claude Code plugin profiles for a developer.\n\n\
         Installed plugins:\n  {}\n\n\
         Suggested profiles (one per detected repo type):\n{}\n\
         Assign each installed plugin to either \"universal\" (loaded in every repo) \
         or to exactly one suggested profile by name. Plugins that fit no profile may be omitted. \
         Use ONLY the plugin keys and profile names listed above. \
         Return the assignment as JSON.",
        plugins.join("\n  "),
        if profiles.is_empty() {
            "  (none)\n".to_string()
        } else {
            profiles
        },
    )
}

/// Parse the `claude -p --output-format json` envelope into an Assignment:
/// prefer the schema-validated `structured_output`; else parse `result` as JSON.
pub fn parse_assignment(stdout: &str) -> Result<Assignment> {
    let env: Value = serde_json::from_str(stdout.trim()).context("parsing claude JSON envelope")?;
    if env.get("is_error").and_then(Value::as_bool) == Some(true) {
        bail!(
            "claude reported an error: {}",
            env.get("result")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        );
    }
    if let Some(so) = env.get("structured_output") {
        if !so.is_null() {
            return serde_json::from_value(so.clone())
                .context("decoding structured_output as Assignment");
        }
    }
    let text = env
        .get("result")
        .and_then(Value::as_str)
        .context("claude envelope has neither structured_output nor a result string")?;
    serde_json::from_str(text).context("decoding result text as Assignment")
}

/// Run headless `claude` to propose a plugin→profile assignment, then validate
/// it against the inventory and assemble a Profiles (detect is scan-derived).
pub fn draft_with_claude(
    inv: &Inventory,
    scan_roots: Vec<String>,
    claude_bin: &Path,
    model: &str,
    timeout_secs: u64,
) -> Result<Profiles> {
    let mut child = crate::util::retry_etxtbsy(|| {
        Command::new(claude_bin)
            .arg("-p")
            .arg(build_prompt(inv))
            .arg("--output-format")
            .arg("json")
            .arg("--json-schema")
            .arg(ASSIGNMENT_SCHEMA)
            .arg("--model")
            .arg(model)
            .arg("--append-system-prompt")
            .arg("Return ONLY the JSON assignment matching the schema. No prose.")
            .arg("--disallowedTools")
            .arg("Bash")
            .arg("Edit")
            .arg("Write")
            .arg("Read")
            // Internal helper call, not a work session — keep cortex's
            // SessionEnd hook from recording a Raw for it.
            .env("CORTEX_SKIP_RECORD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })
    .context("spawning claude -p")?;

    // Drain stdout on a concurrent reader thread so the OS pipe buffer never
    // fills (classic deadlock: child blocks on pipe write; parent blocks in
    // try_wait).  The main thread continues to poll try_wait for the timeout.
    let mut stdout = child.stdout.take().context("claude stdout")?;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });

    // Drain stderr concurrently for the same deadlock reason: if stderr fills
    // the pipe buffer, the child blocks on write while we block in try_wait.
    // We capture it to surface in error messages rather than discarding it.
    let mut stderr = child.stderr.take().context("claude stderr")?;
    let err_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    // wait with timeout (poll; kill on expiry).  On timeout: kill → reap →
    // join reader threads (pipe closes on child death so they return promptly)
    // → bail.  prime.rs uses the same kill+wait pattern in wait_with_timeout.
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("polling claude")? {
            if !status.success() {
                // Join stderr reader and include a snippet in the error so the
                // toast is actionable (e.g. "Invalid API key · Please run /login").
                let stderr_text = err_reader.join().unwrap_or_default();
                let snippet = stderr_text.trim();
                // Take at most the last ~200 chars to keep the message terse.
                let snippet = if snippet.len() > 200 {
                    snippet[snippet.len() - 200..].trim_start()
                } else {
                    snippet
                };
                if snippet.is_empty() {
                    bail!("claude -p exited with {:?}", status.code());
                } else {
                    bail!("claude -p exited with {:?}: {}", status.code(), snippet);
                }
            }
            break;
        }
        if start.elapsed() >= std::time::Duration::from_secs(timeout_secs) {
            let _ = child.kill();
            let _ = child.wait(); // reap zombie — Drop does NOT wait
            let _ = reader.join(); // pipe closes on child death; returns promptly
            let _ = err_reader.join();
            bail!("claude -p timed out after {timeout_secs}s");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = reader
        .join()
        .map_err(|_| anyhow::anyhow!("claude stdout reader thread panicked"))?;
    // Discard stderr on success; join to avoid leaking the thread.
    let _ = err_reader.join();

    let assign = parse_assignment(&out)?;
    validate(&assign, inv)?;
    Ok(assemble_profiles(&assign, inv, scan_roots))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};

    fn inv() -> Inventory {
        Inventory {
            plugins: vec![
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "ra@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![SuggestedProfile {
                name: "rust".into(),
                repos: vec![],
                shared_signals: SharedSignals {
                    marker_files: vec!["Cargo.toml".into()],
                    ..Default::default()
                },
            }],
        }
    }

    /// Write a fake `claude` executable that ignores its args and echoes the
    /// given canned stdout, then returns its path.
    fn fake_claude(dir: &std::path::Path, stdout: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude");
        std::fs::write(&p, format!("#!/bin/sh\ncat <<'EOF'\n{stdout}\nEOF\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn parse_assignment_reads_structured_output() {
        let env = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","structured_output":{"universal":["serena@x"],"profiles":{"rust":["ra@x"]}}}"#;
        let a = parse_assignment(env).unwrap();
        assert_eq!(a.universal, vec!["serena@x".to_string()]);
        assert_eq!(a.profiles["rust"], vec!["ra@x".to_string()]);
    }

    #[test]
    fn parse_assignment_falls_back_to_result_text() {
        let env = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"universal\":[\"serena@x\"],\"profiles\":{}}"}"#;
        let a = parse_assignment(env).unwrap();
        assert_eq!(a.universal, vec!["serena@x".to_string()]);
    }

    #[test]
    fn draft_with_claude_assembles_validated_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let env = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","structured_output":{"universal":["serena@x"],"profiles":{"rust":["ra@x"]}}}"#;
        let bin = fake_claude(dir.path(), env);
        let cfg = draft_with_claude(&inv(), vec!["/r".into()], &bin, DEFAULT_MODEL, 30).unwrap();
        assert_eq!(cfg.universal, vec!["serena@x".to_string()]);
        assert_eq!(cfg.profiles["rust"].plugins, vec!["ra@x".to_string()]);
        // detect is scan-derived, NOT from the AI:
        assert_eq!(
            cfg.profiles["rust"].detect.marker_files,
            vec!["Cargo.toml".to_string()]
        );
    }

    #[test]
    fn draft_with_claude_marks_session_skip_record_for_cortex() {
        // The one-shot draft call is an internal helper, not a work session —
        // CORTEX_SKIP_RECORD=1 keeps cortex's SessionEnd hook from writing a
        // junk Raw for it. The fake claude fails hard when the marker is missing.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let env = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","structured_output":{"universal":["serena@x"],"profiles":{"rust":["ra@x"]}}}"#;
        let p = dir.path().join("claude");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\n\
                 test \"$CORTEX_SKIP_RECORD\" = 1 || {{ echo not-marked >&2; exit 5; }}\n\
                 cat <<'EOF'\n{env}\nEOF\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = draft_with_claude(&inv(), vec!["/r".into()], &p, DEFAULT_MODEL, 30)
            .expect("draft must set CORTEX_SKIP_RECORD=1 on its claude -p");
        assert_eq!(cfg.universal, vec!["serena@x".to_string()]);
    }

    #[test]
    fn draft_with_claude_rejects_uninstalled_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let env = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","structured_output":{"universal":["ghost@x"],"profiles":{}}}"#;
        let bin = fake_claude(dir.path(), env);
        let err = draft_with_claude(&inv(), vec!["/r".into()], &bin, DEFAULT_MODEL, 30)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ghost@x"),
            "validation rejects uninstalled key: {err}"
        );
    }

    #[test]
    fn parse_assignment_bails_on_error_envelope() {
        let env = r#"{"type":"result","subtype":"error","is_error":true,"result":"model refused to produce JSON"}"#;
        let err = parse_assignment(env).unwrap_err().to_string();
        assert!(
            err.contains("model refused to produce JSON"),
            "error text should surface in the bail message: {err}"
        );
    }

    /// A fake `claude` that writes a recognisable message to stderr and exits 1.
    /// Verifies that `draft_with_claude` surfaces the stderr snippet in the error
    /// instead of producing an unactionable "exited with Some(1)" toast.
    fn fake_claude_stderr_exit1(dir: &std::path::Path, stderr_msg: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude");
        std::fs::write(&p, format!("#!/bin/sh\necho '{stderr_msg}' >&2\nexit 1\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn draft_with_claude_surfaces_stderr_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_claude_stderr_exit1(dir.path(), "Invalid API key · Please run /login");
        let err = draft_with_claude(&inv(), vec!["/r".into()], &bin, DEFAULT_MODEL, 30)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("login") || err.contains("Invalid API key"),
            "stderr snippet must appear in the error: {err}"
        );
    }
}
