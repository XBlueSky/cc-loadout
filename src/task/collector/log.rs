//! The collector runner's own log: one fixed-code line per event on cc-loadout's
//! stderr, which a cron entry (or the Geralt reporter's host) appends to the
//! shared task log. Codes, keys and values form a closed table; a value is only
//! ever an attempt ID, a fixed reason or status word, or an integer. Nothing
//! that came from the launcher, Claude Code or a user-supplied path can reach a
//! line, because this module has no parameter that could carry it.

use std::io::Write;

use serde::Serialize;

use crate::task::collector::export::{SafeErrorCode, Status};

/// Prefix of every line, so the collector's lines are easy to pick out of a log
/// that other cc-loadout runs share.
const PREFIX: &str = "cc-loadout-collector";

/// Why an attempt stopped before a launcher was started. The words are the
/// closed list the log may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrelaunchReason {
    InvalidTaskId,
    UnknownTask,
    InvalidTask,
    UnknownAccount,
    StateUnreadable,
    IsolatedDir,
    UnsupportedPlatform,
    LockIo,
}

impl PrelaunchReason {
    /// The word this reason is written as.
    pub fn as_str(self) -> &'static str {
        match self {
            PrelaunchReason::InvalidTaskId => "invalid_task_id",
            PrelaunchReason::UnknownTask => "unknown_task",
            PrelaunchReason::InvalidTask => "invalid_task",
            PrelaunchReason::UnknownAccount => "unknown_account",
            PrelaunchReason::StateUnreadable => "state_unreadable",
            PrelaunchReason::IsolatedDir => "isolated_dir",
            PrelaunchReason::UnsupportedPlatform => "unsupported_platform",
            PrelaunchReason::LockIo => "lock_io",
        }
    }
}

/// One line of the runner log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLine<'a> {
    /// `tasks.json` has no collector section.
    Unconfigured,
    /// The collector section is present but fails its validation.
    SettingsInvalid,
    /// The attempt stopped before a launcher was started.
    Prelaunch {
        attempt: &'a str,
        reason: PrelaunchReason,
    },
    /// Another run holds the cc-loadout lock.
    Busy { attempt: &'a str },
    /// The launcher process was started.
    Started { attempt: &'a str },
    /// The attempt's export record was written.
    Recorded {
        attempt: &'a str,
        status: Status,
        code: Option<SafeErrorCode>,
    },
    /// Processes of the launcher's tree were still alive after the attempt.
    Residual { attempt: &'a str, count: usize },
    /// Launcher stderr lines were counted but not forwarded.
    StderrDropped { attempt: &'a str, lines: usize },
    /// The attempt's export record could not be written.
    ExportFailed { attempt: &'a str },
}

/// The snake_case word `v` serializes to, so the log and the export record can
/// never disagree about how a status or code is spelled.
fn wire_word<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        // Unreachable for the unit enums used here; never echo anything else.
        _ => String::from("unknown"),
    }
}

impl LogLine<'_> {
    /// The line, without its trailing newline.
    pub fn render(&self) -> String {
        match self {
            LogLine::Unconfigured => format!("{PREFIX} collector_unconfigured"),
            LogLine::SettingsInvalid => format!("{PREFIX} collector_settings_invalid"),
            LogLine::Prelaunch { attempt, reason } => format!(
                "{PREFIX} prelaunch_failed attempt={attempt} reason={}",
                reason.as_str()
            ),
            LogLine::Busy { attempt } => format!("{PREFIX} lock_busy attempt={attempt}"),
            LogLine::Started { attempt } => format!("{PREFIX} attempt_started attempt={attempt}"),
            LogLine::Recorded {
                attempt,
                status,
                code,
            } => format!(
                "{PREFIX} attempt_recorded attempt={attempt} status={} safe_error={}",
                wire_word(status),
                code.as_ref()
                    .map_or_else(|| String::from("none"), wire_word)
            ),
            LogLine::Residual { attempt, count } => {
                format!("{PREFIX} tree_residual attempt={attempt} count={count}")
            }
            LogLine::StderrDropped { attempt, lines } => {
                format!("{PREFIX} launcher_stderr_dropped attempt={attempt} lines={lines}")
            }
            LogLine::ExportFailed { attempt } => {
                format!("{PREFIX} export_write_failed attempt={attempt}")
            }
        }
    }

    /// Write the line to stderr. A closed or full stderr is ignored: logging
    /// must never decide how an attempt ends.
    pub fn emit(&self) {
        let _ = writeln!(std::io::stderr().lock(), "{}", self.render());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_every_line_from_the_closed_table() {
        let cases: Vec<(LogLine, &str)> = vec![
            (
                LogLine::Unconfigured,
                "cc-loadout-collector collector_unconfigured",
            ),
            (
                LogLine::SettingsInvalid,
                "cc-loadout-collector collector_settings_invalid",
            ),
            (
                LogLine::Prelaunch {
                    attempt: "att-1",
                    reason: PrelaunchReason::UnknownTask,
                },
                "cc-loadout-collector prelaunch_failed attempt=att-1 reason=unknown_task",
            ),
            (
                LogLine::Busy { attempt: "att-1" },
                "cc-loadout-collector lock_busy attempt=att-1",
            ),
            (
                LogLine::Started { attempt: "att-1" },
                "cc-loadout-collector attempt_started attempt=att-1",
            ),
            (
                LogLine::Recorded {
                    attempt: "att-1",
                    status: Status::SkippedBusy,
                    code: Some(SafeErrorCode::LockBusy),
                },
                "cc-loadout-collector attempt_recorded attempt=att-1 status=skipped_busy safe_error=lock_busy",
            ),
            (
                LogLine::Recorded {
                    attempt: "att-1",
                    status: Status::ExitedOk,
                    code: None,
                },
                "cc-loadout-collector attempt_recorded attempt=att-1 status=exited_ok safe_error=none",
            ),
            (
                LogLine::Residual {
                    attempt: "att-1",
                    count: 3,
                },
                "cc-loadout-collector tree_residual attempt=att-1 count=3",
            ),
            (
                LogLine::StderrDropped {
                    attempt: "att-1",
                    lines: 12,
                },
                "cc-loadout-collector launcher_stderr_dropped attempt=att-1 lines=12",
            ),
            (
                LogLine::ExportFailed { attempt: "att-1" },
                "cc-loadout-collector export_write_failed attempt=att-1",
            ),
        ];
        for (line, want) in cases {
            assert_eq!(line.render(), want);
        }
    }

    #[test]
    fn every_prelaunch_reason_has_its_closed_word() {
        let table = [
            (PrelaunchReason::InvalidTaskId, "invalid_task_id"),
            (PrelaunchReason::UnknownTask, "unknown_task"),
            (PrelaunchReason::InvalidTask, "invalid_task"),
            (PrelaunchReason::UnknownAccount, "unknown_account"),
            (PrelaunchReason::StateUnreadable, "state_unreadable"),
            (PrelaunchReason::IsolatedDir, "isolated_dir"),
            (PrelaunchReason::UnsupportedPlatform, "unsupported_platform"),
            (PrelaunchReason::LockIo, "lock_io"),
        ];
        for (reason, word) in table {
            assert_eq!(reason.as_str(), word);
            assert_eq!(
                LogLine::Prelaunch {
                    attempt: "a",
                    reason
                }
                .render(),
                format!("cc-loadout-collector prelaunch_failed attempt=a reason={word}")
            );
        }
    }

    #[test]
    fn status_and_code_words_are_the_export_words() {
        // Same spelling as the export record, for every status and every code.
        let statuses = [
            Status::ExitedOk,
            Status::ExitedError,
            Status::TimedOut,
            Status::SkippedBusy,
            Status::Killed,
            Status::LaunchFailed,
        ];
        for s in statuses {
            let line = LogLine::Recorded {
                attempt: "a",
                status: s,
                code: None,
            }
            .render();
            let want = serde_json::to_value(s).unwrap();
            assert!(
                line.contains(&format!("status={} ", want.as_str().unwrap())),
                "{line}"
            );
        }
        for c in [
            SafeErrorCode::AssembleFailed,
            SafeErrorCode::ProcessTreeResidual,
            SafeErrorCode::TurnLimit,
        ] {
            let line = LogLine::Recorded {
                attempt: "a",
                status: Status::ExitedError,
                code: Some(c),
            }
            .render();
            let want = serde_json::to_value(c).unwrap();
            assert!(
                line.ends_with(&format!("safe_error={}", want.as_str().unwrap())),
                "{line}"
            );
        }
    }

    #[test]
    fn a_rendered_line_is_one_line_of_the_closed_alphabet() {
        // Whatever the variant, a line has no newline and only plain ASCII
        // words, `=` and `_`, so it cannot be turned into two log entries.
        let lines = [
            LogLine::Unconfigured.render(),
            LogLine::Busy { attempt: "att-x" }.render(),
            LogLine::Residual {
                attempt: "att-x",
                count: 7,
            }
            .render(),
        ];
        for l in lines {
            assert!(l.starts_with("cc-loadout-collector "));
            assert!(l
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'_' | b'-' | b'=')));
        }
    }
}
