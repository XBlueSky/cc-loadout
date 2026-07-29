//! One-time removal of the `settings.json` hook entries older cc-loadout
//! versions installed from `install.sh`.
//!
//! Those entries embed an absolute path into whatever clone ran the installer,
//! and nothing ever removed them — uninstalling the plugin or moving the clone
//! left a silently-failing hook behind forever. The plugin now owns the hooks,
//! so these must go. This is the one write cc-loadout still makes to
//! `settings.json`, and it is self-terminating: once removed, every later call
//! is a no-op that touches nothing.

use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

use crate::util::atomicfile;

const EVENTS: [&str; 2] = ["SessionStart", "SessionEnd"];

/// True for cc-loadout's own retired hook commands, and nothing else.
///
/// Two shapes exist in the wild: the `lib/session-{start,end}-hook.sh` wrapper,
/// and the older inline one-liner that sourced `registry.sh` and called
/// `promote_universal_to_user`. All three markers are anchored strictly to avoid
/// false positives: the hook path deletion runs automatically on every session
/// start once the plugin hook is wired up, and a mis-identified entry is
/// silently deleted from `~/.claude/settings.json`, a file shared by all plugins.
/// A false negative leaves one of our zombies behind; a false positive destroys
/// someone else's hook and never surfaces a diagnostic. Asymmetric risk demands
/// strict matching on all branches.
///
/// A plugin-relative command (containing `${CLAUDE_PLUGIN_ROOT}`) is never ours.
/// Our installer wrote absolute paths derived from the clone, long before
/// plugin-owned hooks existed. If that variable appears, the entry belongs to
/// another plugin, so reject it immediately.
///
/// The wrapper scripts anchor to `/lib/` with a leading slash to enforce path
/// segment boundaries (always present in our installer paths like
/// `/clone/lib/session-start-hook.sh`, never in legitimate uses like
/// `/opt/other/mylib/session-start-hook.sh`). The inline form requires both
/// `registry.sh` and `promote_universal_to_user` (a collision with both markers
/// is implausible).
pub fn is_legacy_command(command: &str) -> bool {
    // A plugin-relative command is never ours. install.sh wrote an
    // absolute path derived from the clone, and did so long before
    // plugin-owned hooks existed, so ${CLAUDE_PLUGIN_ROOT} appearing at
    // all means this entry belongs to some other plugin.
    if command.contains("CLAUDE_PLUGIN_ROOT") {
        return false;
    }
    command.contains("/lib/session-start-hook.sh")
        || command.contains("/lib/session-end-hook.sh")
        || (command.contains("registry.sh") && command.contains("promote_universal_to_user"))
}

fn strip(root: &mut Value) -> usize {
    let mut removed = 0usize;
    let hooks = match root.get_mut("hooks").and_then(Value::as_object_mut) {
        Some(h) => h,
        None => return 0,
    };
    for event in EVENTS {
        let groups = match hooks.get_mut(event).and_then(Value::as_array_mut) {
            Some(g) => g,
            None => continue,
        };
        for group in groups.iter_mut() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|h| {
                    !is_legacy_command(h.get("command").and_then(Value::as_str).unwrap_or(""))
                });
                removed += before - list.len();
            }
        }
        // A group whose hooks list is now empty carries no meaning.
        groups.retain(|g| {
            g.get("hooks")
                .and_then(Value::as_array)
                .map(|l| !l.is_empty())
                .unwrap_or(false)
        });
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    removed
}

/// `Ok(None)` means the file is absent — a normal state on a machine that never
/// ran an older installer. Every other failure (unreadable file, malformed
/// JSON) propagates, per the "Absent is not unreadable" constraint. The hook
/// path swallows the error at its call site because a hook must never block a
/// session; `doctor` surfaces it, because a diagnostic that reports health
/// during a real failure is worse than no diagnostic at all.
fn load(settings_path: &Path) -> Result<Option<Value>> {
    let bytes = match std::fs::read(settings_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", settings_path.display())),
    };
    let root = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", settings_path.display()))?;
    Ok(Some(root))
}

/// How many retired entries `settings_path` still holds. Never writes.
pub fn count_legacy_hooks(settings_path: &Path) -> Result<usize> {
    match load(settings_path)? {
        Some(mut root) => Ok(strip(&mut root)),
        None => Ok(0),
    }
}

/// Remove the retired entries. Returns how many went; 0 means the file was not
/// touched. An absent `settings.json` yields 0; one that exists but cannot be
/// read or parsed is an error.
pub fn remove_legacy_hooks(settings_path: &Path) -> Result<usize> {
    let mut root = match load(settings_path)? {
        Some(r) => r,
        None => return Ok(0),
    };
    let removed = strip(&mut root);
    if removed > 0 {
        // Back up before the only write cc-loadout makes to a file it does not
        // own. Best-effort insurance: the write proceeds even if the copy fails,
        // but when it succeeds a mis-identified entry is recoverable instead of
        // gone. Single fixed name, per the plan's backup policy.
        let _ = std::fs::copy(settings_path, atomicfile::sidecar_backup(settings_path));
        let out = serde_json::to_vec_pretty(&root)?;
        atomicfile::write_atomic(settings_path, &out, 0o644)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTHER_PLUGIN: &str =
        r#"bash ${CLAUDE_PLUGIN_ROOT}/hooks/scripts/session-start-inject.sh"#;

    fn settings_with(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, body).unwrap();
        (dir, p)
    }

    #[test]
    fn recognises_every_retired_cc_loadout_command_shape() {
        assert!(is_legacy_command(r#"bash "/x/lib/session-start-hook.sh""#));
        assert!(is_legacy_command(r#"bash "/x/lib/session-end-hook.sh""#));
        assert!(is_legacy_command(
            r#"bash -c 'source /x/lib/registry.sh && promote_universal_to_user'"#
        ));
    }

    #[test]
    fn leaves_other_plugins_hooks_alone() {
        assert!(!is_legacy_command(OTHER_PLUGIN));
        assert!(!is_legacy_command("bash /x/lib/registry.sh"));
    }

    #[test]
    fn removes_only_cc_loadout_entries_and_keeps_the_neighbour() {
        let (_d, p) = settings_with(&format!(
            r#"{{"hooks":{{"SessionStart":[
                {{"hooks":[{{"type":"command","command":"bash \"/x/lib/session-start-hook.sh\""}}]}},
                {{"hooks":[{{"type":"command","command":"{OTHER_PLUGIN}"}}]}}
            ]}}}}"#
        ));
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 1);

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        let groups = v["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "the emptied cc-loadout group is dropped");
        assert_eq!(groups[0]["hooks"][0]["command"], OTHER_PLUGIN);
    }

    #[test]
    fn preserves_an_entry_with_no_command_field_while_removing_a_legacy_one() {
        // A hook entry need not have a `command` field at all (e.g. a
        // `type: "prompt"` hook). `is_legacy_command("")` must be false, so
        // `strip()` leaves such an entry alone instead of mistaking "absent"
        // for "matches" and dropping someone else's configuration. Go through
        // `remove_legacy_hooks` rather than `strip()` directly, so this proves
        // the entry survives a real write, not just the in-memory filter.
        const PROMPT_ONLY: &str = r#"{"type": "prompt", "prompt": "some prompt"}"#;
        let (_d, p) = settings_with(&format!(
            r#"{{"hooks":{{"SessionStart":[
                {{"hooks":[
                    {{"type":"command","command":"bash \"/x/lib/session-start-hook.sh\""}},
                    {PROMPT_ONLY}
                ]}}
            ]}}}}"#
        ));
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 1);

        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        let hooks = v["hooks"]["SessionStart"][0]["hooks"].as_array().unwrap();
        assert_eq!(
            hooks.len(),
            1,
            "the legacy entry is removed, the command-less one stays"
        );
        assert_eq!(hooks[0]["type"], "prompt");
        assert!(hooks[0].get("command").is_none());
    }

    #[test]
    fn drops_the_event_key_when_it_becomes_empty() {
        let (_d, p) = settings_with(
            r#"{"hooks":{"SessionEnd":[
                {"hooks":[{"type":"command","command":"bash \"/x/lib/session-end-hook.sh\""}]}
            ]}}"#,
        );
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 1);
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert!(v["hooks"].get("SessionEnd").is_none());
    }

    #[test]
    fn is_idempotent_and_does_not_rewrite_a_clean_file() {
        let (_d, p) = settings_with(&format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{OTHER_PLUGIN}"}}]}}]}}}}"#
        ));
        let before = std::fs::read_to_string(&p).unwrap();
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn a_missing_settings_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            remove_legacy_hooks(&dir.path().join("nope.json")).unwrap(),
            0
        );
        assert_eq!(
            count_legacy_hooks(&dir.path().join("nope.json")).unwrap(),
            0
        );
    }

    #[test]
    fn a_malformed_settings_file_propagates_as_an_error() {
        // Absent is not unreadable: a settings.json that exists but cannot be
        // parsed is a real problem `doctor` must report, not a silent zero.
        let (_d, p) = settings_with("not json");
        assert!(remove_legacy_hooks(&p).is_err());
        assert!(count_legacy_hooks(&p).is_err());
    }

    #[test]
    fn count_does_not_mutate() {
        let (_d, p) = settings_with(
            r#"{"hooks":{"SessionStart":[
                {"hooks":[{"type":"command","command":"bash \"/x/lib/session-start-hook.sh\""}]}
            ]}}"#,
        );
        let before = std::fs::read_to_string(&p).unwrap();
        assert_eq!(count_legacy_hooks(&p).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
    }

    #[test]
    fn anchors_on_lib_to_reject_same_basename_different_path() {
        // False positives would silently delete another plugin's hook.
        // Our installer always wrote /path/lib/session-{start,end}-hook.sh.
        // Another plugin writing /path/scripts/session-start-hook.sh must not match.
        assert!(!is_legacy_command(
            r#"bash "${CLAUDE_PLUGIN_ROOT}/scripts/session-start-hook.sh""#
        ));
        assert!(!is_legacy_command(
            r#"bash "/opt/other-plugin/session-end-hook.sh""#
        ));
        // The early return rejects plugin-relative commands entirely: our installer
        // wrote absolute paths derived from the clone, so ${CLAUDE_PLUGIN_ROOT} at
        // all means another plugin. This guards both the wrapper-script and
        // inline-registry branches.
        assert!(!is_legacy_command(
            r#"bash "${CLAUDE_PLUGIN_ROOT}/lib/session-start-hook.sh""#
        ));
        assert!(!is_legacy_command(
            r#"bash -c 'source ${CLAUDE_PLUGIN_ROOT}/lib/registry.sh && promote_universal_to_user'"#
        ));
        // Directory boundary: /mylib/session-start-hook.sh must not match /lib/
        // anchor, ensuring lib is a whole path segment, not a suffix.
        assert!(!is_legacy_command(
            r#"bash "/opt/other/mylib/session-start-hook.sh""#
        ));
    }

    #[test]
    fn creates_exactly_one_fixed_name_backup_on_removal() {
        let (_d, p) = settings_with(
            r#"{"hooks":{"SessionStart":[
                {"hooks":[{"type":"command","command":"bash \"/x/lib/session-start-hook.sh\""}]}
            ]}}"#,
        );
        let dir = p.parent().unwrap();
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 1);

        let baks: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .collect();
        assert_eq!(baks.len(), 1, "exactly one fixed-name backup on removal");
    }

    #[test]
    fn does_not_create_backup_on_clean_file() {
        let (_d, p) = settings_with(&format!(
            r#"{{"hooks":{{"SessionStart":[{{"hooks":[{{"type":"command","command":"{OTHER_PLUGIN}"}}]}}]}}}}"#
        ));
        let dir = p.parent().unwrap();
        assert_eq!(remove_legacy_hooks(&p).unwrap(), 0);

        let baks: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .collect();
        assert_eq!(baks.len(), 0, "no backup when file is not modified");
    }
}
