//! `cc-loadout doctor` — inspect and repair cc-loadout's own installation.
//!
//! This is the recovery path for the one failure mode plugin-owned hooks
//! cannot self-heal: if `cc-loadout@cc-loadout` itself drifts to `scope: local`
//! it stops loading outside the repo it is bound to, so its own SessionStart
//! hook stops running there. `doctor --fix` puts it back.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// The seed template, embedded rather than read from disk: a binary installed
/// by `curl | bash` has no repo to copy `profiles.example.json` from.
const PROFILES_TEMPLATE: &str = include_str!("../profiles.example.json");

#[derive(Debug, Default)]
pub struct DoctorReport {
    pub seeded_profiles: bool,
    /// Keys this run actually promoted (`--fix` only).
    pub promoted: Vec<String>,
    /// Keys that lack a user-scope entry (reported when not fixing).
    pub needs_promotion: Vec<String>,
    /// Retired `settings.json` entries found (and removed when fixing).
    pub legacy_hooks: usize,
    pub stale_backups: Vec<PathBuf>,
    pub pruned_backups: usize,
}

/// Timestamped registry backups left by cc-loadout versions before the
/// fixed-name scheme. One machine accumulated 108 of these (1.7 MB).
fn find_stale_backups(registry_path: &Path) -> Vec<PathBuf> {
    let dir = match registry_path.parent() {
        Some(d) => d,
        None => return Vec::new(),
    };
    let prefix = format!(
        "{}.bak.",
        registry_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    );
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

pub fn run(
    home: &Path,
    config_override: Option<&Path>,
    fix: bool,
    prune_backups: bool,
) -> Result<DoctorReport> {
    let mut report = DoctorReport::default();

    let cfg_path = crate::profile::config::profiles_path(home);
    if !cfg_path.exists() {
        report.seeded_profiles = true;
        if fix {
            crate::util::atomicfile::write_atomic(&cfg_path, PROFILES_TEMPLATE.as_bytes(), 0o644)?;
        }
    }

    let registry = crate::profile::discover::resolve_registry_path(home, config_override);
    if let Ok(cfg) = crate::profile::config::load(&cfg_path) {
        if fix {
            let r = crate::profile::registry::promote_all(&cfg, &registry)?;
            report.promoted = r.promoted;
        } else {
            report.needs_promotion =
                crate::profile::registry::keys_needing_promotion(&cfg, &registry);
        }
    }

    let settings = crate::hooks::settings_path(home, config_override);
    report.legacy_hooks = if fix {
        crate::hooks::legacy::remove_legacy_hooks(&settings)?
    } else {
        crate::hooks::legacy::count_legacy_hooks(&settings)?
    };

    // Both files accumulated timestamped backups from the retired shell
    // installer: `installed_plugins.json.bak.<epoch>` from promote_keys_to_user
    // and `settings.json.bak.<epoch>` from install_session_hook. Reclaiming
    // only the first would leave the user staring at the other half forever.
    report.stale_backups = find_stale_backups(&registry);
    report.stale_backups.extend(find_stale_backups(&settings));
    report.stale_backups.sort();
    if fix && prune_backups {
        for p in &report.stale_backups {
            if std::fs::remove_file(p).is_ok() {
                report.pruned_backups += 1;
            }
        }
    }

    Ok(report)
}

pub fn print(report: &DoctorReport, fix: bool) {
    if report.seeded_profiles {
        println!(
            "{} profiles.json (absent)",
            if fix { "seeded" } else { "would seed" }
        );
    }
    if fix {
        if report.promoted.is_empty() {
            println!("plugin scope: already consistent");
        } else {
            println!("promoted to scope: user ({}):", report.promoted.len());
            for k in &report.promoted {
                println!("  {k}");
            }
        }
    } else if report.needs_promotion.is_empty() {
        println!("plugin scope: already consistent");
    } else {
        println!("needs scope: user ({}):", report.needs_promotion.len());
        for k in &report.needs_promotion {
            println!("  {k}");
        }
    }
    if report.legacy_hooks > 0 {
        println!(
            "{} {} retired settings.json hook entr{}",
            if fix { "removed" } else { "found" },
            report.legacy_hooks,
            if report.legacy_hooks == 1 { "y" } else { "ies" }
        );
    }
    if !report.stale_backups.is_empty() {
        if report.pruned_backups > 0 {
            println!("pruned {} stale registry backup(s)", report.pruned_backups);
        } else {
            println!(
                "{} stale registry backup(s) reclaimable with --prune-backups",
                report.stale_backups.len()
            );
        }
    }
    if !fix {
        println!("\n(run with --fix to apply)");
    }
}
