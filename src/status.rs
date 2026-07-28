//! Read-only combined view of every loadout slot.
//!
//! This is the single seam where the slots "meet". It only READS each slot and
//! renders two independent sections (account, profile). It writes nothing,
//! switches nothing, and shares NO derived field across the two sections — a
//! joint verdict over both slots would be coupling and is deliberately absent.

use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use time::OffsetDateTime;

use crate::account::paths::ClaudePaths;
use crate::account::store::Store;
use crate::profile::config::Profiles;

#[derive(Serialize)]
struct StatusJson {
    account: AccountSectionJson,
    profile: ProfileSectionJson,
    priming: std::collections::BTreeMap<String, crate::account::json::PrimingEntryJson>,
}

#[derive(Serialize)]
struct AccountSectionJson {
    accounts: Vec<crate::account::json::AccountJson>,
}

#[derive(Serialize)]
struct ProfileSectionJson {
    cwd: String,
    profiles_json: bool,
    matched: Vec<String>,
    applied: Vec<String>,
}

/// Print the account section followed by the profile section for `cwd`.
/// `cfg` is `None` when no profiles.json exists. `json` switches to the
/// machine-readable envelope.
pub fn show(
    store: &Store,
    claude: &ClaudePaths,
    cfg: Option<&Profiles>,
    cwd: &Path,
    now_ms: i64,
    now_local: OffsetDateTime,
    json: bool,
) -> Result<()> {
    let rows = crate::account::list(store, claude)?;
    let schedule = crate::task::ops::load_prime_times(store.data_root())?;
    let state = store.load_state()?;

    if json {
        let accounts = rows
            .iter()
            .map(|r| crate::account::json::account_json(r, now_ms))
            .collect();
        let profile = match cfg {
            None => ProfileSectionJson {
                cwd: cwd.display().to_string(),
                profiles_json: false,
                matched: Vec::new(),
                applied: Vec::new(),
            },
            Some(cfg) => ProfileSectionJson {
                cwd: cwd.display().to_string(),
                profiles_json: true,
                matched: crate::profile::detect::detect_profiles(cwd, cfg),
                applied: crate::profile::apply::enabled_keys(cwd)?,
            },
        };
        return crate::json::emit(&StatusJson {
            account: AccountSectionJson { accounts },
            profile,
            priming: crate::account::json::priming_json(&schedule, &state, now_local),
        });
    }

    // ---- account slot (machine-global) ----
    println!("Account:");
    if rows.is_empty() {
        println!("  (none — run: cc-loadout account add <alias>)");
    } else {
        for r in &rows {
            let marker = if r.is_active { "*" } else { " " };
            let org = r.meta.org_name.as_deref().unwrap_or("personal");
            let token = crate::account::render_token_status(r.expires_at_ms, r.has_refresh, now_ms);
            println!(
                "{marker} {:<12} {} [{}] token:{token}",
                r.alias, r.meta.email, org
            );
        }
    }

    // ---- profile slot (per-repo) ----
    println!("\nProfile (cwd: {}):", cwd.display());
    match cfg {
        None => println!("  (no profiles.json — run: cc-loadout profile init)"),
        Some(cfg) => {
            let matched = crate::profile::detect::detect_profiles(cwd, cfg);
            let label = if matched.is_empty() {
                "(none — default)".to_string()
            } else {
                matched.join(" ")
            };
            println!("  Matched profiles: {label}");
            match crate::profile::apply::current_enabled(cwd)? {
                None => println!("  Applied here: (no settings.local.json / no enabledPlugins)"),
                Some(v) => {
                    let on = v
                        .as_object()
                        .map(|o| o.values().filter(|x| x.as_bool() == Some(true)).count())
                        .unwrap_or(0);
                    println!(
                        "  Applied here: {on} plugin(s) enabled in .claude/settings.local.json"
                    );
                }
            }
        }
    }

    // ---- priming slot (scheduled accounts only) ----
    if !schedule.is_empty() {
        println!("\nPriming:");
        let now_epoch = now_ms / 1000;
        for (alias, times) in &schedule {
            print!("  {alias}:");
            if let Some(nf) = crate::account::timing::next_fire(times, now_local) {
                print!(" next {} {:02}:{:02}", nf.date(), nf.hour(), nf.minute());
            }
            match state.accounts.get(alias).and_then(|m| m.last_primed) {
                Some(lp) => {
                    let ago = crate::account::format_duration_short((now_epoch - lp) * 1000)
                        .unwrap_or_else(|| "just now".into());
                    println!(", last primed {ago} ago");
                }
                None => println!(", last primed never"),
            }
        }
    }

    Ok(())
}
