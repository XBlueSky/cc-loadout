//! What the launcher's stdout and exit code mean for the attempt's export record:
//! the one outcome line it may print, and the table that turns its exit code plus
//! the runner's own observations into a status and a safe error code. The table
//! mirrors Geralt's `LAUNCHER_EXIT` / `exitToExecution` (`src/launcher/outcome.ts`)
//! and the code-to-status table of `execution-contract.ts`, which the Geralt
//! server enforces on every record.

use serde_json::Value;

use crate::task::collector::export::{SafeErrorCode, Status};

/// Value of the `schema` key of the launcher's outcome line.
pub const OUTCOME_SCHEMA: &str = "geralt.launch-outcome";
/// How much of the launcher's stdout is kept. The outcome line is far smaller; an
/// output that does not fit is not an outcome line.
pub const STDOUT_CAP_BYTES: usize = 4096;

/// The `version` key of the outcome line.
const OUTCOME_VERSION: f64 = 1.0;
/// Longest model string, in UTF-16 code units (JavaScript's `String#length`).
const MODEL_MAX_UTF16_UNITS: usize = 200;

/// The three values of a valid outcome line, ready to copy into the export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub requested_model: Option<String>,
    pub effective_model: Option<String>,
    pub effective_policy_digest: Option<String>,
}

/// Parse the launcher's stdout the way Geralt's `parseOutcomeLine` does: exactly
/// one line (one trailing `\n` is allowed, anything else after or before it is
/// not), UTF-8, a JSON object with exactly the five keys `schema`, `version`,
/// `requestedModel`, `effectiveModel` and `effectivePolicyDigest`. A rejected
/// input yields `None` and nothing else: it is never echoed anywhere.
pub fn parse_outcome_line(bytes: &[u8]) -> Option<Outcome> {
    let text = std::str::from_utf8(bytes).ok()?;
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.contains('\n') {
        return None;
    }
    // A `Value` rather than a derived struct: a derived `Option` field would
    // accept a missing key as `null`, and the line must carry all five.
    let Value::Object(obj) = serde_json::from_str(body).ok()? else {
        return None;
    };
    if obj.len() != 5 {
        return None;
    }
    if obj.get("schema")?.as_str()? != OUTCOME_SCHEMA {
        return None;
    }
    if obj.get("version")?.as_f64()? != OUTCOME_VERSION {
        return None;
    }
    Some(Outcome {
        requested_model: model_value(obj.get("requestedModel")?)?,
        effective_model: model_value(obj.get("effectiveModel")?)?,
        effective_policy_digest: digest_value(obj.get("effectivePolicyDigest")?)?,
    })
}

/// A model field: `null`, or a string that is 1 to 200 UTF-16 code units long after
/// trimming and has no NUL. The outer `None` is "not acceptable", the inner one is
/// `null`.
fn model_value(v: &Value) -> Option<Option<String>> {
    let s = match v {
        Value::Null => return Some(None),
        Value::String(s) => js_trim(s),
        _ => return None,
    };
    let units = s.encode_utf16().count();
    if units == 0 || units > MODEL_MAX_UTF16_UNITS || s.contains('\0') {
        return None;
    }
    Some(Some(s.to_string()))
}

/// A digest field: `null` or `sha256:` followed by 64 lower-case hex digits.
fn digest_value(v: &Value) -> Option<Option<String>> {
    let s = match v {
        Value::Null => return Some(None),
        Value::String(s) => s,
        _ => return None,
    };
    let hex = s.strip_prefix("sha256:")?;
    let ok = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    ok.then(|| Some(s.clone()))
}

/// `String#trim` of JavaScript, which is what Geralt applies before it measures a
/// model: the ECMAScript WhiteSpace and LineTerminator sets. Not `str::trim`,
/// whose set differs (it strips U+0085 and keeps U+FEFF), so that a value both
/// sides call empty is empty on both.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    })
}

/// How the runner saw the launcher process end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observation {
    /// It exited with this code.
    Exited(i32),
    /// It was ended by a signal that nobody here accounts for as a timeout or a
    /// kill.
    Signaled,
    /// It could not be started; `enoent` is true when the program or its working
    /// directory does not exist.
    SpawnError { enoent: bool },
    /// Waiting for it failed, so its end was never seen.
    NotReaped,
}

/// What the runner itself did to the attempt, whatever the launcher reported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Observed {
    /// The runner's wall timer fired and it asked the launcher to stop.
    pub timed_out: bool,
    /// The runner was told to stop the attempt from outside and did so.
    pub killed: bool,
    /// Processes of the launcher's tree outlived the attempt.
    pub residual: bool,
}

/// The status, codes and model fields an attempt's record carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub status: Status,
    pub safe_error_code: Option<SafeErrorCode>,
    pub exit_code: Option<i32>,
    pub requested_model: Option<String>,
    pub effective_model: Option<String>,
    pub effective_policy_digest: Option<String>,
}

/// Turn what the runner saw into the record's classification.
///
/// The launcher's exit code decides the row of the contract's table (code 30 and
/// every code outside the table defer to the runner's own observation); the
/// runner's timer or kill wins over that table. A leftover process tree only
/// changes the safe error code of an `exited_ok`, `timed_out` or `killed`
/// attempt: every other status already carries the launcher's own reason.
pub fn classify(obs: &Observation, seen: &Observed, outcome: Option<Outcome>) -> Classified {
    let (status, mut safe_error_code, exit_code) = match *obs {
        Observation::SpawnError { enoent } => {
            let code = if enoent {
                SafeErrorCode::SpawnEnoent
            } else {
                SafeErrorCode::SpawnFailed
            };
            (Status::LaunchFailed, Some(code), None)
        }
        Observation::Exited(code) => match runner_decided(seen) {
            Some(status) => (status, None, Some(code)),
            None => launcher_row(code),
        },
        Observation::Signaled | Observation::NotReaped => (
            runner_decided(seen).unwrap_or(Status::ExitedError),
            None,
            None,
        ),
    };

    if seen.residual && matches!(status, Status::ExitedOk | Status::TimedOut | Status::Killed) {
        safe_error_code = Some(SafeErrorCode::ProcessTreeResidual);
    }

    // Nothing ran after a spawn error, so there is no outcome to believe. After
    // exit 2-9 the launcher stopped before it knew an effective model or policy:
    // a value it printed for them anyway is not copied.
    let outcome = match obs {
        Observation::SpawnError { .. } => None,
        Observation::Exited(2..=9) => outcome.map(|o| Outcome {
            effective_model: None,
            effective_policy_digest: None,
            ..o
        }),
        _ => outcome,
    }
    .unwrap_or(Outcome {
        requested_model: None,
        effective_model: None,
        effective_policy_digest: None,
    });

    Classified {
        status,
        safe_error_code,
        exit_code,
        requested_model: outcome.requested_model,
        effective_model: outcome.effective_model,
        effective_policy_digest: outcome.effective_policy_digest,
    }
}

/// The status the runner's own timer or kill gives the attempt, if it acted. The
/// timer comes first: an attempt that ran out of time and was then also told to
/// stop is a timeout.
fn runner_decided(seen: &Observed) -> Option<Status> {
    if seen.timed_out {
        Some(Status::TimedOut)
    } else if seen.killed {
        Some(Status::Killed)
    } else {
        None
    }
}

/// The row of the contract's launcher-exit table for `code`, as (status, safe
/// error code, record exit code). A launch failure carries no exit code; 30
/// (SIGTERM or SIGINT) and any value outside the table are an `exited_error`
/// without a code, keeping the launcher's actual exit code.
fn launcher_row(code: i32) -> (Status, Option<SafeErrorCode>, Option<i32>) {
    use SafeErrorCode as C;
    let launch_failed = |c: C| (Status::LaunchFailed, Some(c), None);
    match code {
        0 => (Status::ExitedOk, None, Some(0)),
        1 => (Status::ExitedError, Some(C::LauncherError), Some(1)),
        2 => launch_failed(C::ConfigInvalid),
        3 => launch_failed(C::AssembleFailed),
        4 => launch_failed(C::SpawnEnoent),
        5 => launch_failed(C::SpawnFailed),
        6 => launch_failed(C::CliVersionMismatch),
        7 => launch_failed(C::AuthRequired),
        8 => launch_failed(C::PreflightFailed),
        9 => launch_failed(C::ControlProtocolError),
        20 => (Status::ExitedError, Some(C::RuntimeMismatch), Some(20)),
        21 => (Status::ExitedError, Some(C::TurnLimit), Some(21)),
        22 => (Status::ExitedError, Some(C::SessionError), Some(22)),
        other => (Status::ExitedError, None, Some(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn line(requested: &str, effective: &str, digest: &str) -> String {
        format!(
            "{{\"schema\":\"geralt.launch-outcome\",\"version\":1,\"requestedModel\":{requested},\"effectiveModel\":{effective},\"effectivePolicyDigest\":{digest}}}"
        )
    }

    fn good() -> String {
        line("\"m1\"", "\"m2\"", &format!("\"{DIGEST}\""))
    }

    fn full_outcome() -> Outcome {
        Outcome {
            requested_model: Some("m1".into()),
            effective_model: Some("m2".into()),
            effective_policy_digest: Some(DIGEST.into()),
        }
    }

    #[test]
    fn parses_the_exact_launcher_line() {
        let got = parse_outcome_line(format!("{}\n", good()).as_bytes());
        assert_eq!(got, Some(full_outcome()));
    }

    #[test]
    fn the_trailing_newline_is_optional_and_nulls_are_values() {
        assert_eq!(parse_outcome_line(good().as_bytes()), Some(full_outcome()));
        let nulls = line("null", "null", "null");
        assert_eq!(
            parse_outcome_line(format!("{nulls}\n").as_bytes()),
            Some(Outcome {
                requested_model: None,
                effective_model: None,
                effective_policy_digest: None,
            })
        );
    }

    #[test]
    fn key_order_does_not_matter_but_the_key_set_does() {
        let shuffled = format!(
            "{{\"effectivePolicyDigest\":null,\"effectiveModel\":\"b\",\"version\":1,\"requestedModel\":\"a\",\"schema\":\"{OUTCOME_SCHEMA}\"}}\n"
        );
        assert_eq!(
            parse_outcome_line(shuffled.as_bytes()),
            Some(Outcome {
                requested_model: Some("a".into()),
                effective_model: Some("b".into()),
                effective_policy_digest: None,
            })
        );
    }

    #[test]
    fn rejects() {
        let g = good();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty input", Vec::new()),
            ("only a newline", b"\n".to_vec()),
            ("a line before", format!("noise\n{g}\n").into_bytes()),
            ("a line after", format!("{g}\nnoise\n").into_bytes()),
            ("two newlines at the end", format!("{g}\n\n").into_bytes()),
            ("a leading newline", format!("\n{g}\n").into_bytes()),
            (
                "an extra key",
                g.replacen('{', "{\"extra\":1,", 1).into_bytes(),
            ),
            (
                "a missing key",
                format!(
                    "{{\"schema\":\"{OUTCOME_SCHEMA}\",\"version\":1,\"requestedModel\":null,\"effectiveModel\":null}}"
                )
                .into_bytes(),
            ),
            (
                "a missing key replaced by an extra one",
                format!(
                    "{{\"schema\":\"{OUTCOME_SCHEMA}\",\"version\":1,\"requestedModel\":null,\"effectiveModel\":null,\"other\":null}}"
                )
                .into_bytes(),
            ),
            ("version 2", g.replace("\"version\":1", "\"version\":2").into_bytes()),
            (
                "version as a string",
                g.replace("\"version\":1", "\"version\":\"1\"").into_bytes(),
            ),
            (
                "another schema",
                g.replace(OUTCOME_SCHEMA, "geralt.other").into_bytes(),
            ),
            (
                "an upper-case digest",
                line("null", "null", &format!("\"sha256:{}\"", "A".repeat(64))).into_bytes(),
            ),
            (
                "a short digest",
                line("null", "null", &format!("\"sha256:{}\"", "a".repeat(63))).into_bytes(),
            ),
            (
                "a digest without its prefix",
                line("null", "null", &format!("\"{}\"", "a".repeat(64))).into_bytes(),
            ),
            (
                "a digest followed by a newline",
                line("null", "null", &format!("\"sha256:{}\\n\"", "a".repeat(64))).into_bytes(),
            ),
            ("an empty model", line("\"\"", "null", "null").into_bytes()),
            ("a blank model", line("\"  \\t \"", "null", "null").into_bytes()),
            (
                "a model of 201 characters",
                line("null", &format!("\"{}\"", "x".repeat(201)), "null").into_bytes(),
            ),
            (
                "a model with a NUL",
                line("\"a\\u0000b\"", "null", "null").into_bytes(),
            ),
            ("a numeric model", line("5", "null", "null").into_bytes()),
            ("not UTF-8", vec![0xff, 0xfe, b'{', b'}']),
            ("an array", b"[]".to_vec()),
            ("a bare string", b"\"geralt.launch-outcome\"".to_vec()),
            ("not JSON", b"{ nope".to_vec()),
        ];
        for (name, bytes) in cases {
            assert_eq!(parse_outcome_line(&bytes), None, "{name}");
        }
    }

    #[test]
    fn models_are_trimmed_and_measured_like_the_geralt_schema() {
        // `safeText(1, 200)`: trimmed first, then 1..=200 UTF-16 units.
        let got = parse_outcome_line(line("\" m1 \"", "null", "null").as_bytes()).unwrap();
        assert_eq!(got.requested_model.as_deref(), Some("m1"));

        let exactly_200 = "x".repeat(200);
        let got =
            parse_outcome_line(line("null", &format!("\"{exactly_200}\""), "null").as_bytes())
                .unwrap();
        assert_eq!(got.effective_model.as_deref(), Some(exactly_200.as_str()));

        // 100 astral characters are 200 UTF-16 units; 101 are 202.
        let ok = "\u{1F600}".repeat(100);
        assert!(
            parse_outcome_line(line(&format!("\"{ok}\""), "null", "null").as_bytes()).is_some()
        );
        let too_long = "\u{1F600}".repeat(101);
        assert_eq!(
            parse_outcome_line(line(&format!("\"{too_long}\""), "null", "null").as_bytes()),
            None
        );

        // JavaScript's trim removes U+FEFF and keeps U+0085; Rust's is the other
        // way round. A model that is only U+FEFF is empty to Geralt, which would
        // refuse the whole record, so it must not get through here.
        assert_eq!(
            parse_outcome_line(line("\"\\ufeff\"", "null", "null").as_bytes()),
            None
        );
        let got = parse_outcome_line(line("\"\\ufeffm1\\u00a0\"", "null", "null").as_bytes());
        assert_eq!(got.unwrap().requested_model.as_deref(), Some("m1"));
        let nel = parse_outcome_line(line("\"\\u0085\"", "null", "null").as_bytes());
        assert_eq!(nel.unwrap().requested_model.as_deref(), Some("\u{85}"));
    }

    fn obs_exit(c: i32) -> Observation {
        Observation::Exited(c)
    }

    fn calm() -> Observed {
        Observed::default()
    }

    fn row(c: &Classified) -> (Status, Option<SafeErrorCode>, Option<i32>) {
        (c.status, c.safe_error_code, c.exit_code)
    }

    #[test]
    fn classify_is_the_q10_table() {
        use SafeErrorCode as C;
        use Status as S;
        let table: [(i32, S, Option<C>, Option<i32>); 14] = [
            (0, S::ExitedOk, None, Some(0)),
            (1, S::ExitedError, Some(C::LauncherError), Some(1)),
            (2, S::LaunchFailed, Some(C::ConfigInvalid), None),
            (3, S::LaunchFailed, Some(C::AssembleFailed), None),
            (4, S::LaunchFailed, Some(C::SpawnEnoent), None),
            (5, S::LaunchFailed, Some(C::SpawnFailed), None),
            (6, S::LaunchFailed, Some(C::CliVersionMismatch), None),
            (7, S::LaunchFailed, Some(C::AuthRequired), None),
            (8, S::LaunchFailed, Some(C::PreflightFailed), None),
            (9, S::LaunchFailed, Some(C::ControlProtocolError), None),
            (20, S::ExitedError, Some(C::RuntimeMismatch), Some(20)),
            (21, S::ExitedError, Some(C::TurnLimit), Some(21)),
            (22, S::ExitedError, Some(C::SessionError), Some(22)),
            (30, S::ExitedError, None, Some(30)),
        ];
        for (code, status, safe, exit) in table {
            let got = classify(&obs_exit(code), &calm(), None);
            assert_eq!(row(&got), (status, safe, exit), "launcher exit {code}");
        }
        // Outside the table: the runner has nothing to add, the code is kept.
        for code in [10, 19, 23, 29, 31, 99, 127, 255] {
            let got = classify(&obs_exit(code), &calm(), None);
            assert_eq!(
                row(&got),
                (Status::ExitedError, None, Some(code)),
                "launcher exit {code}"
            );
        }
    }

    #[test]
    fn classify_spawn_errors() {
        let enoent = classify(&Observation::SpawnError { enoent: true }, &calm(), None);
        assert_eq!(
            row(&enoent),
            (Status::LaunchFailed, Some(SafeErrorCode::SpawnEnoent), None)
        );
        let other = classify(&Observation::SpawnError { enoent: false }, &calm(), None);
        assert_eq!(
            row(&other),
            (Status::LaunchFailed, Some(SafeErrorCode::SpawnFailed), None)
        );
        // Nothing ran: whatever outcome there might be is not copied, and a
        // leftover tree does not change a launch failure.
        let seen = Observed {
            residual: true,
            ..calm()
        };
        let got = classify(
            &Observation::SpawnError { enoent: true },
            &seen,
            Some(full_outcome()),
        );
        assert_eq!(
            row(&got),
            (Status::LaunchFailed, Some(SafeErrorCode::SpawnEnoent), None)
        );
        assert_eq!(
            (
                got.requested_model,
                got.effective_model,
                got.effective_policy_digest
            ),
            (None, None, None)
        );
    }

    #[test]
    fn classify_signal_and_runner_observation() {
        let timed_out = Observed {
            timed_out: true,
            ..calm()
        };
        let killed = Observed {
            killed: true,
            ..calm()
        };
        let both = Observed {
            timed_out: true,
            killed: true,
            ..calm()
        };
        let cases = vec![
            (
                "30 + timer",
                obs_exit(30),
                timed_out,
                (Status::TimedOut, Some(30)),
            ),
            (
                "30 + kill",
                obs_exit(30),
                killed,
                (Status::Killed, Some(30)),
            ),
            (
                "30 alone",
                obs_exit(30),
                calm(),
                (Status::ExitedError, Some(30)),
            ),
            (
                "timer wins over kill",
                obs_exit(30),
                both,
                (Status::TimedOut, Some(30)),
            ),
            (
                "signaled alone",
                Observation::Signaled,
                calm(),
                (Status::ExitedError, None),
            ),
            (
                "signaled + timer",
                Observation::Signaled,
                timed_out,
                (Status::TimedOut, None),
            ),
            (
                "signaled + kill",
                Observation::Signaled,
                killed,
                (Status::Killed, None),
            ),
            (
                "not reaped + timer",
                Observation::NotReaped,
                timed_out,
                (Status::TimedOut, None),
            ),
            (
                "not reaped + kill",
                Observation::NotReaped,
                killed,
                (Status::Killed, None),
            ),
            (
                "not reaped alone",
                Observation::NotReaped,
                calm(),
                (Status::ExitedError, None),
            ),
            // The launcher finished in its own right as the timer fired: the
            // runner's observation still decides, and its exit code is kept.
            (
                "0 + timer",
                obs_exit(0),
                timed_out,
                (Status::TimedOut, Some(0)),
            ),
            (
                "22 + kill",
                obs_exit(22),
                killed,
                (Status::Killed, Some(22)),
            ),
        ];
        for (name, obs, seen, (status, exit)) in cases {
            let got = classify(&obs, &seen, None);
            assert_eq!(row(&got), (status, None, exit), "{name}");
        }
    }

    #[test]
    fn classify_residual() {
        let residual = Observed {
            residual: true,
            ..calm()
        };
        let residual_timed_out = Observed {
            timed_out: true,
            ..residual
        };
        let residual_killed = Observed {
            killed: true,
            ..residual
        };
        let res = Some(SafeErrorCode::ProcessTreeResidual);
        let got = classify(&obs_exit(0), &residual, None);
        assert_eq!(row(&got), (Status::ExitedOk, res, Some(0)));
        let got = classify(&obs_exit(30), &residual_timed_out, None);
        assert_eq!(row(&got), (Status::TimedOut, res, Some(30)));
        let got = classify(&Observation::Signaled, &residual_killed, None);
        assert_eq!(row(&got), (Status::Killed, res, None));
        // A launcher reason stays: the leftover tree is only a log line then.
        let got = classify(&obs_exit(22), &residual, None);
        assert_eq!(
            row(&got),
            (
                Status::ExitedError,
                Some(SafeErrorCode::SessionError),
                Some(22)
            )
        );
        let got = classify(&obs_exit(7), &residual, None);
        assert_eq!(
            row(&got),
            (
                Status::LaunchFailed,
                Some(SafeErrorCode::AuthRequired),
                None
            )
        );
        let got = classify(&obs_exit(99), &residual, None);
        assert_eq!(row(&got), (Status::ExitedError, None, Some(99)));
        let got = classify(&Observation::Signaled, &residual, None);
        assert_eq!(row(&got), (Status::ExitedError, None, None));
    }

    #[test]
    fn classify_models() {
        let models = |c: &Classified| {
            (
                c.requested_model.clone(),
                c.effective_model.clone(),
                c.effective_policy_digest.clone(),
            )
        };
        // Exit 2-9: only the requested model is believed.
        for code in 2..=9 {
            let got = classify(&obs_exit(code), &calm(), Some(full_outcome()));
            assert_eq!(models(&got), (Some("m1".into()), None, None), "exit {code}");
        }
        // Every other exit copies the outcome as parsed.
        for code in [0, 1, 20, 21, 22, 30, 99] {
            let got = classify(&obs_exit(code), &calm(), Some(full_outcome()));
            assert_eq!(
                models(&got),
                (
                    Some("m1".into()),
                    Some("m2".into()),
                    Some(DIGEST.to_string())
                ),
                "exit {code}"
            );
        }
        // No outcome line: nothing to copy.
        for code in [0, 5, 22, 30] {
            let got = classify(&obs_exit(code), &calm(), None);
            assert_eq!(models(&got), (None, None, None), "exit {code}");
        }
        // The rule follows the launcher's code, not the status the runner chose.
        let timed_out = Observed {
            timed_out: true,
            ..calm()
        };
        let got = classify(&obs_exit(8), &timed_out, Some(full_outcome()));
        assert_eq!(got.status, Status::TimedOut);
        assert_eq!(models(&got), (Some("m1".into()), None, None));
        // Partial outcomes copy field by field.
        let partial = Outcome {
            requested_model: None,
            effective_model: Some("m2".into()),
            effective_policy_digest: None,
        };
        let got = classify(&obs_exit(0), &calm(), Some(partial));
        assert_eq!(models(&got), (None, Some("m2".into()), None));
    }

    /// Geralt's `SAFE_ERROR_CODE_STATUSES`, spelled out independently.
    fn allowed_statuses(code: SafeErrorCode) -> &'static [Status] {
        use SafeErrorCode as C;
        match code {
            C::AssembleFailed
            | C::AuthRequired
            | C::CliVersionMismatch
            | C::ConfigInvalid
            | C::ControlProtocolError
            | C::PreflightFailed
            | C::SpawnEnoent
            | C::SpawnFailed => &[Status::LaunchFailed],
            C::LauncherError | C::RuntimeMismatch | C::SessionError | C::TurnLimit => {
                &[Status::ExitedError]
            }
            C::LockBusy => &[Status::SkippedBusy],
            C::ProcessTreeResidual => &[Status::ExitedOk, Status::TimedOut, Status::Killed],
        }
    }

    #[test]
    fn every_classification_passes_the_q3_cross_rules() {
        let mut observations = vec![
            Observation::Signaled,
            Observation::NotReaped,
            Observation::SpawnError { enoent: true },
            Observation::SpawnError { enoent: false },
        ];
        observations.extend((-1..=40).map(Observation::Exited));
        observations.extend([99, 127, 128, 137, 143, 255].map(Observation::Exited));
        let outcomes = [None, Some(full_outcome())];
        let flags = [false, true];

        let mut count = 0;
        for obs in &observations {
            for timed_out in flags {
                for killed in flags {
                    for residual in flags {
                        for outcome in &outcomes {
                            let seen = Observed {
                                timed_out,
                                killed,
                                residual,
                            };
                            let got = classify(obs, &seen, outcome.clone());
                            let ctx = format!("{obs:?} {seen:?} outcome={}", outcome.is_some());
                            count += 1;

                            // Rule 1 and the code-to-status table: a code only
                            // sits with the statuses it allows, and an
                            // `exited_ok` carries no code but the residual one.
                            if let Some(code) = got.safe_error_code {
                                assert!(
                                    allowed_statuses(code).contains(&got.status),
                                    "{ctx}: {code:?} with {:?}",
                                    got.status
                                );
                            }
                            if got.status == Status::ExitedOk {
                                assert!(
                                    matches!(
                                        got.safe_error_code,
                                        None | Some(SafeErrorCode::ProcessTreeResidual)
                                    ),
                                    "{ctx}"
                                );
                            }
                            // Rule 2: an `exited_ok` has exit code 0 (or none).
                            if got.status == Status::ExitedOk {
                                assert!(matches!(got.exit_code, None | Some(0)), "{ctx}");
                            }
                            // Rule 3: an `exited_error` is never exit code 0.
                            if got.status == Status::ExitedError {
                                assert_ne!(got.exit_code, Some(0), "{ctx}");
                            }
                            // Rule 4: a launch failure or a busy skip has no
                            // exit code.
                            if matches!(got.status, Status::LaunchFailed | Status::SkippedBusy) {
                                assert_eq!(got.exit_code, None, "{ctx}");
                            }
                            // Rule 5 compares the two timestamps, which the run
                            // path takes in order; classify has no clock.

                            // The two statuses a classification never produces.
                            assert_ne!(got.status, Status::SkippedBusy, "{ctx}");
                            assert_ne!(got.safe_error_code, Some(SafeErrorCode::LockBusy), "{ctx}");
                        }
                    }
                }
            }
        }
        assert_eq!(count, (4 + 42 + 6) * 8 * 2);
    }
}
