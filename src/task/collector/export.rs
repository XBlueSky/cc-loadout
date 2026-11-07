//! Execution-record export v1: the one file per attempt that Geralt's reporter
//! reads. Field names, order, closed code lists and the 16 KiB cap mirror
//! Geralt's `src/reporter/export-schema.ts` and `execution-contract.ts`; this
//! module only writes, it never reads a record back.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use time::{OffsetDateTime, UtcOffset};

/// Value of the `schema` key of every record.
pub const EXPORT_RECORD_SCHEMA: &str = "geralt.execution-record";
/// Value of the `version` key of every record.
pub const EXPORT_SCHEMA_VERSION: u32 = 1;
/// Largest record Geralt accepts, in bytes of the file as written (trailing
/// newline included). A larger one is refused before any file is created.
pub const MAX_EXPORT_RECORD_BYTES: usize = 16384;

/// How the attempt ended. The closed list is Geralt's `EXECUTION_STATUSES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    ExitedOk,
    ExitedError,
    TimedOut,
    SkippedBusy,
    Killed,
    LaunchFailed,
}

/// Why the attempt did not end plainly. The closed list is Geralt's
/// `SAFE_ERROR_CODE_LIST`; each code is only valid with the statuses Geralt's
/// code-to-status table lists, which the caller must respect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeErrorCode {
    AssembleFailed,
    AuthRequired,
    CliVersionMismatch,
    ConfigInvalid,
    ControlProtocolError,
    LauncherError,
    LockBusy,
    PreflightFailed,
    ProcessTreeResidual,
    RuntimeMismatch,
    SessionError,
    SpawnEnoent,
    SpawnFailed,
    TurnLimit,
}

/// One execution record. Fields serialize in declaration order (the order of
/// the contract's JSON example) and every `None` serializes as an explicit
/// `null`, never as a missing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRecord {
    pub schema: &'static str,
    pub version: u32,
    pub runner_instance_id: String,
    pub task_id: String,
    pub task_revision: Option<String>,
    pub attempt_id: String,
    pub record_revision: u32,
    pub session_id: Option<String>,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub requested_model: Option<String>,
    pub effective_model: Option<String>,
    pub effective_policy_digest: Option<String>,
    pub safe_error_code: Option<SafeErrorCode>,
}

impl ExportRecord {
    /// A record for an attempt that never ran a launcher: constants for
    /// `schema` and `version`, `record_revision` 1, every optional field null.
    pub fn not_launched(
        runner: &str,
        task: &str,
        attempt: &str,
        status: Status,
        code: Option<SafeErrorCode>,
    ) -> Self {
        Self {
            schema: EXPORT_RECORD_SCHEMA,
            version: EXPORT_SCHEMA_VERSION,
            runner_instance_id: runner.to_string(),
            task_id: task.to_string(),
            task_revision: None,
            attempt_id: attempt.to_string(),
            record_revision: 1,
            session_id: None,
            status,
            exit_code: None,
            started_at: None,
            ended_at: None,
            requested_model: None,
            effective_model: None,
            effective_policy_digest: None,
            safe_error_code: code,
        }
    }
}

/// A fresh attempt ID: `att-` plus a random UUID, 40 characters of
/// `[a-z0-9-]`, which fits Geralt's export attempt-ID grammar.
pub fn new_attempt_id() -> String {
    format!("att-{}", uuid::Uuid::new_v4())
}

/// `t` in UTC as `YYYY-MM-DDTHH:MM:SS.mmmZ`, sub-millisecond digits truncated:
/// the shape of JavaScript's `Date#toISOString`. Meant for years 0000-9999.
pub fn format_instant(t: OffsetDateTime) -> String {
    let t = t.to_offset(UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// File name of one record: `<attempt>.<revision>.json`.
pub fn export_file_name(attempt_id: &str, record_revision: u32) -> String {
    format!("{attempt_id}.{record_revision}.json")
}

/// Geralt's export attempt-ID grammar, `^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,199}$`.
/// It has no `/` and cannot start with `.`, so a name built from it stays in
/// the export directory and never looks like a temporary file.
fn is_export_attempt_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 200 || !(b[0].is_ascii_alphanumeric() || b[0] == b'_') {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

/// Write `rec` into `dir` as `<attempt>.<revision>.json` and return its path.
///
/// Order: serialize (compact JSON plus a trailing newline), refuse a record
/// over [`MAX_EXPORT_RECORD_BYTES`] before touching the disk, create `dir`
/// (mode 0700, only the components that are missing), write a `.`-prefixed
/// temporary file (0600) in `dir`, sync it, link it to its final name without
/// replacing anything, then sync `dir`. A reader that skips dotfiles never sees
/// a partial record, and an existing record is never overwritten. If syncing
/// the directory fails the record is already in place and the error still
/// comes back.
pub fn write_export(dir: &Path, rec: &ExportRecord) -> Result<PathBuf> {
    if !is_export_attempt_id(&rec.attempt_id) {
        bail!("export attempt id does not fit the export file-name grammar");
    }
    let mut body = serde_json::to_vec(rec).context("serializing the export record")?;
    body.push(b'\n');
    if body.len() > MAX_EXPORT_RECORD_BYTES {
        bail!(
            "export record is {} bytes, over the {MAX_EXPORT_RECORD_BYTES} byte limit",
            body.len()
        );
    }
    let target = dir.join(export_file_name(&rec.attempt_id, rec.record_revision));

    // Only missing components are created (0700); an existing directory keeps its mode.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating export directory {}", dir.display()))?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".")
        .tempfile_in(dir)
        .with_context(|| format!("creating a temporary file in {}", dir.display()))?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .context("setting the temporary export file to mode 0600")?;
    tmp.write_all(&body)
        .context("writing the temporary export file")?;
    tmp.as_file()
        .sync_all()
        .context("syncing the temporary export file")?;
    // Fails if `target` exists; the temporary file is then removed on drop.
    tmp.persist_noclobber(&target)
        .map_err(|e| e.error)
        .with_context(|| format!("placing export file {}", target.display()))?;

    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing export directory {}", dir.display()))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    /// `^[A-Za-z0-9_][A-Za-z0-9_.:-]{0,199}$`, spelled out independently of the
    /// implementation.
    fn matches_attempt_pattern(s: &str) -> bool {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > 200 {
            return false;
        }
        (b[0].is_ascii_alphanumeric() || b[0] == b'_')
            && b[1..]
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
    }

    fn mode_of(p: &Path) -> u32 {
        fs::metadata(p).unwrap().mode() & 0o7777
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn sample() -> ExportRecord {
        ExportRecord::not_launched("runner-a", "task-a", "att-x", Status::LaunchFailed, None)
    }

    #[test]
    fn serializes_all_16_keys_in_contract_order_with_explicit_nulls() {
        let bytes = serde_json::to_vec(&sample()).unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let obj = v.as_object().unwrap();
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "schema",
                "version",
                "runnerInstanceId",
                "taskId",
                "taskRevision",
                "attemptId",
                "recordRevision",
                "sessionId",
                "status",
                "exitCode",
                "startedAt",
                "endedAt",
                "requestedModel",
                "effectiveModel",
                "effectivePolicyDigest",
                "safeErrorCode",
            ]
        );
        assert_eq!(obj["taskRevision"], Value::Null);
        for k in [
            "sessionId",
            "exitCode",
            "startedAt",
            "endedAt",
            "requestedModel",
            "effectiveModel",
            "effectivePolicyDigest",
            "safeErrorCode",
        ] {
            assert_eq!(obj[k], Value::Null, "{k} must be an explicit null");
        }
        assert_eq!(obj["schema"], "geralt.execution-record");
        assert_eq!(obj["version"], 1);
        assert_eq!(obj["recordRevision"], 1);
        assert_eq!(obj["status"], "launch_failed");
        assert_eq!(obj["runnerInstanceId"], "runner-a");
        assert_eq!(obj["taskId"], "task-a");
        assert_eq!(obj["attemptId"], "att-x");
    }

    #[test]
    fn status_and_codes_serialize_to_the_closed_lists() {
        let statuses = [
            (Status::ExitedOk, "exited_ok"),
            (Status::ExitedError, "exited_error"),
            (Status::TimedOut, "timed_out"),
            (Status::SkippedBusy, "skipped_busy"),
            (Status::Killed, "killed"),
            (Status::LaunchFailed, "launch_failed"),
        ];
        for (s, want) in statuses {
            assert_eq!(serde_json::to_value(s).unwrap(), want);
        }
        let codes = [
            (SafeErrorCode::AssembleFailed, "assemble_failed"),
            (SafeErrorCode::AuthRequired, "auth_required"),
            (SafeErrorCode::CliVersionMismatch, "cli_version_mismatch"),
            (SafeErrorCode::ConfigInvalid, "config_invalid"),
            (
                SafeErrorCode::ControlProtocolError,
                "control_protocol_error",
            ),
            (SafeErrorCode::LauncherError, "launcher_error"),
            (SafeErrorCode::LockBusy, "lock_busy"),
            (SafeErrorCode::PreflightFailed, "preflight_failed"),
            (SafeErrorCode::ProcessTreeResidual, "process_tree_residual"),
            (SafeErrorCode::RuntimeMismatch, "runtime_mismatch"),
            (SafeErrorCode::SessionError, "session_error"),
            (SafeErrorCode::SpawnEnoent, "spawn_enoent"),
            (SafeErrorCode::SpawnFailed, "spawn_failed"),
            (SafeErrorCode::TurnLimit, "turn_limit"),
        ];
        assert_eq!(codes.len(), 14);
        for (c, want) in codes {
            assert_eq!(serde_json::to_value(c).unwrap(), want);
        }
    }

    #[test]
    fn attempt_id_matches_export_attempt_id_pattern() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let id = new_attempt_id();
            assert!(matches_attempt_pattern(&id), "bad attempt id {id}");
            assert!(id.starts_with("att-"));
            assert!(seen.insert(id), "attempt ids must be unique");
        }
    }

    #[test]
    fn format_instant_is_utc_millis_z() {
        let t = time::Date::from_calendar_date(2026, time::Month::November, 7)
            .unwrap()
            .with_hms_nano(3, 4, 5, 123_456_789)
            .unwrap()
            .assume_offset(UtcOffset::from_hms(8, 0, 0).unwrap());
        assert_eq!(format_instant(t), "2026-11-06T19:04:05.123Z");

        let shape = |s: &str| {
            let b = s.as_bytes();
            b.len() == 24
                && b.iter().enumerate().all(|(i, c)| match i {
                    4 | 7 => *c == b'-',
                    10 => *c == b'T',
                    13 | 16 => *c == b':',
                    19 => *c == b'.',
                    23 => *c == b'Z',
                    _ => c.is_ascii_digit(),
                })
        };
        for _ in 0..100 {
            let s = format_instant(OffsetDateTime::now_utc());
            assert!(shape(&s), "bad instant {s}");
        }
    }

    #[test]
    fn export_file_name_is_attempt_dot_revision_dot_json() {
        assert_eq!(export_file_name("att-x", 1), "att-x.1.json");
        assert_eq!(export_file_name("att-x", 12), "att-x.12.json");
    }

    #[test]
    fn write_export_creates_dir_0700_and_file_0600_and_leaves_no_dotfile() {
        let t = tempfile::tempdir().unwrap();
        let outer = t.path().join("outer");
        let dir = outer.join("exports");
        let rec = sample();

        let path = write_export(&dir, &rec).unwrap();

        assert_eq!(path, dir.join("att-x.1.json"));
        assert_eq!(mode_of(&dir), 0o700);
        assert_eq!(mode_of(&outer), 0o700);
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(entries(&dir), ["att-x.1.json"]);

        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
        let mut expected = serde_json::to_vec(&rec).unwrap();
        expected.push(b'\n');
        assert_eq!(bytes, expected);
    }

    #[test]
    fn write_export_keeps_existing_dir_mode() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("exports");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).unwrap();

        let path = write_export(&dir, &sample()).unwrap();

        assert_eq!(mode_of(&dir), 0o750);
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(entries(&dir), ["att-x.1.json"]);
    }

    #[test]
    fn write_export_refuses_to_overwrite() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("exports");
        let path = write_export(&dir, &sample()).unwrap();
        let before = fs::read(&path).unwrap();

        let mut second = sample();
        second.status = Status::Killed;
        let err = write_export(&dir, &second).unwrap_err();

        assert!(
            format!("{err:#}").contains("att-x.1.json"),
            "error should name the file: {err:#}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(entries(&dir), ["att-x.1.json"]);
    }

    #[test]
    fn write_export_rejects_records_over_16384_bytes() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("exports");
        fs::create_dir(&dir).unwrap();

        let mut rec = sample();
        rec.requested_model = Some("x".repeat(20000));
        write_export(&dir, &rec).unwrap_err();
        assert!(entries(&dir).is_empty());

        // The size check comes first: not even a missing directory is created.
        let missing = t.path().join("never-created");
        write_export(&missing, &rec).unwrap_err();
        assert!(!missing.exists());
    }

    #[test]
    fn write_export_limit_is_exactly_16384_bytes_including_the_newline() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("exports");

        let mut rec = sample();
        rec.requested_model = Some(String::new());
        let base = serde_json::to_vec(&rec).unwrap().len() + 1;
        let pad = MAX_EXPORT_RECORD_BYTES - base;

        rec.requested_model = Some("a".repeat(pad + 1));
        write_export(&dir, &rec).unwrap_err();
        assert!(!dir.exists());

        rec.requested_model = Some("a".repeat(pad));
        let path = write_export(&dir, &rec).unwrap();
        assert_eq!(fs::read(&path).unwrap().len(), MAX_EXPORT_RECORD_BYTES);
    }

    #[test]
    fn write_export_rejects_an_attempt_id_that_is_not_a_plain_file_name() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("exports");
        fs::create_dir(&dir).unwrap();

        for bad in ["../evil", "a/b", ".hidden", "", "-x", "a b"] {
            let mut rec = sample();
            rec.attempt_id = bad.to_string();
            write_export(&dir, &rec).unwrap_err();
        }
        assert!(entries(&dir).is_empty());
        assert!(entries(t.path()) == ["exports"], "nothing escaped the dir");
    }
}
