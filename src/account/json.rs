#![allow(dead_code)]
use serde::Serialize;
use std::collections::BTreeMap;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::account::store::{AccountMeta, State};
use crate::account::timing::next_fire;
use crate::account::{classify, AccountStatus, TokenStatus};

/// Token sub-object for `account list` / `status`.
#[derive(Serialize)]
pub struct TokenJson {
    pub status: &'static str,
    pub expires_at_ms: Option<i64>,
    pub has_refresh: bool,
}

/// One account row in `--json`.
#[derive(Serialize)]
pub struct AccountJson {
    pub alias: String,
    pub email: String,
    pub org: String,
    pub active: bool,
    pub token: TokenJson,
    pub last_used: Option<i64>,
    pub last_primed: Option<i64>,
}

#[derive(Serialize)]
pub struct AccountListJson {
    pub accounts: Vec<AccountJson>,
}

#[derive(Serialize)]
pub struct CurrentJson {
    pub active: Option<String>,
}

#[derive(Serialize)]
pub struct PrimeJson {
    pub alias: String,
    pub outcome: &'static str,
    pub last_primed: Option<i64>,
}

#[derive(Serialize)]
pub struct ScheduleListJson {
    pub schedule: BTreeMap<String, Vec<String>>,
    pub next_fire: BTreeMap<String, Option<String>>,
    pub last_primed: BTreeMap<String, Option<i64>>,
}

/// `schedule list --json` payload: the schedule plus per-alias RFC3339 next-fire
/// (local) and the stored last_primed epoch.
pub fn schedule_list_json(
    schedule: &BTreeMap<String, Vec<String>>,
    state: &State,
    now: OffsetDateTime,
) -> ScheduleListJson {
    let mut next = BTreeMap::new();
    let mut last = BTreeMap::new();
    for (alias, times) in schedule {
        next.insert(
            alias.clone(),
            next_fire(times, now).and_then(|t| t.format(&Rfc3339).ok()),
        );
        last.insert(
            alias.clone(),
            state.accounts.get(alias).and_then(|m| m.last_primed),
        );
    }
    ScheduleListJson {
        schedule: schedule.clone(),
        next_fire: next,
        last_primed: last,
    }
}

/// Per-account priming timing entry for `status --json`.
#[derive(Serialize)]
pub struct PrimingEntryJson {
    pub next_fire: Option<String>,
    pub last_primed: Option<i64>,
}

/// Per-scheduled-account priming timing for `status --json` (keyed by alias).
pub fn priming_json(
    schedule: &BTreeMap<String, Vec<String>>,
    state: &State,
    now: OffsetDateTime,
) -> BTreeMap<String, PrimingEntryJson> {
    schedule
        .iter()
        .map(|(alias, times)| {
            (
                alias.clone(),
                PrimingEntryJson {
                    next_fire: next_fire(times, now).and_then(|t| t.format(&Rfc3339).ok()),
                    last_primed: state.accounts.get(alias).and_then(|m| m.last_primed),
                },
            )
        })
        .collect()
}

/// Map the token-status enum to its stable JSON string.
pub fn token_status_str(s: TokenStatus) -> &'static str {
    match s {
        TokenStatus::Ok => "ok",
        TokenStatus::Refreshable => "refreshable",
        TokenStatus::Expired => "expired",
        TokenStatus::Unknown => "unknown",
    }
}

/// "personal" when no org name is recorded (mirrors the human renderers).
pub fn org_label(meta: &AccountMeta) -> String {
    meta.org_name
        .clone()
        .unwrap_or_else(|| "personal".to_string())
}

/// Build one `AccountJson` from an `AccountStatus` row (the same data
/// `account list` already computes). `now_ms` drives token classification.
pub fn account_json(r: &AccountStatus, now_ms: i64) -> AccountJson {
    AccountJson {
        alias: r.alias.clone(),
        email: r.meta.email.clone(),
        org: org_label(&r.meta),
        active: r.is_active,
        token: TokenJson {
            status: token_status_str(classify(r.expires_at_ms, r.has_refresh, now_ms)),
            expires_at_ms: r.expires_at_ms,
            has_refresh: r.has_refresh,
        },
        last_used: r.meta.last_used,
        last_primed: r.meta.last_primed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::store::AccountMeta;

    fn row(active: bool, exp: Option<i64>, refresh: bool) -> AccountStatus {
        AccountStatus {
            alias: "work".into(),
            meta: AccountMeta {
                email: "a@b.com".into(),
                org_uuid: None,
                org_name: None,
                added_at: 1,
                last_used: Some(1700),
                last_primed: None,
            },
            is_active: active,
            expires_at_ms: exp,
            has_refresh: refresh,
        }
    }

    #[test]
    fn account_json_maps_fields_and_token_status() {
        let now = 1_000_000;
        let j = account_json(&row(true, Some(now + 60_000), true), now);
        assert_eq!(j.alias, "work");
        assert_eq!(j.org, "personal");
        assert!(j.active);
        assert_eq!(j.token.status, "ok");
        assert_eq!(j.token.expires_at_ms, Some(now + 60_000));
        assert_eq!(j.last_used, Some(1700));
        assert_eq!(j.last_primed, None);
    }

    #[test]
    fn token_status_strings() {
        let now = 1_000_000;
        assert_eq!(
            account_json(&row(false, Some(now - 1), true), now)
                .token
                .status,
            "refreshable"
        );
        assert_eq!(
            account_json(&row(false, Some(now - 1), false), now)
                .token
                .status,
            "expired"
        );
        assert_eq!(
            account_json(&row(false, None, false), now).token.status,
            "unknown"
        );
    }

    #[test]
    fn payloads_serialize_through_envelope() {
        let j = AccountListJson { accounts: vec![] };
        let s = crate::json::to_string(&j).unwrap();
        assert!(s.contains("\"schema_version\": 1"));
        assert!(s.contains("\"accounts\""));
    }
}
