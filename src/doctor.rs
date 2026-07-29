//! `cc-loadout doctor` — inspect and repair cc-loadout's own installation.
//!
//! This is the recovery path for the one failure mode plugin-owned hooks
//! cannot self-heal: if `cc-loadout@cc-loadout` itself drifts to `scope: local`
//! it stops loading outside the repo it is bound to, so its own SessionStart
//! hook stops running there. `doctor --fix` puts it back.

use anyhow::{Context, Result};
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
///
/// The suffix after `.bak.` must be entirely ASCII digits — the retired
/// installer always wrote `.bak.<epoch>` — so a user's own sidecar such as
/// `settings.json.bak.before-migration` is never swept up and, under
/// `--fix --prune-backups`, deleted. Absence of the containing directory is
/// not an error (a fresh install has neither `~/.claude/plugins/` nor
/// `~/.claude/settings.json` yet); any other read failure propagates, same
/// rule as everywhere else in this module.
fn find_stale_backups(path: &Path) -> Result<Vec<PathBuf>> {
    let dir = match path.parent() {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };
    let prefix = format!(
        "{}.bak.",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry =
            entry.with_context(|| format!("reading a directory entry in {}", dir.display()))?;
        let entry_path = entry.path();
        let name = entry_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let is_stale_backup = name
            .as_deref()
            .and_then(|n| n.strip_prefix(prefix.as_str()))
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()));
        if is_stale_backup {
            out.push(entry_path);
        }
    }
    out.sort();
    Ok(out)
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

    // Explicit `Option`, not `if let Ok(cfg) = load(...)`: that idiom is correct
    // in `hooks::session_start` (a hook must never block a session) but wrong
    // here — it would swallow a corrupt profiles.json exactly like an absent
    // one, and `doctor` is the one place a diagnostic must not report health
    // during a real failure. `cfg_path.exists()` is re-checked (not cached from
    // above) because `--fix` may have just seeded it in the block above.
    let cfg = if cfg_path.exists() {
        Some(
            crate::profile::config::load(&cfg_path)
                .with_context(|| format!("loading profiles from {}", cfg_path.display()))?,
        )
    } else {
        None
    };

    let registry = crate::profile::discover::resolve_registry_path(home, config_override);
    // Probed once, unconditionally, before the fix/read-only split below:
    // `keys_needing_promotion` is built on the infallible `discover::list_plugins`
    // (empty vec on a corrupt file, indistinguishable from "nothing installed"),
    // while `promote_all` does propagate a parse error. Without this probe the
    // two paths would disagree about the same corrupt file.
    crate::profile::registry::probe_registry(&registry)?;
    if let Some(cfg) = cfg {
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

    // Three files accumulate timestamped backups: `installed_plugins.json.bak.<epoch>`
    // and `settings.json.bak.<epoch>` from the retired shell installer, plus
    // `profiles.json.bak.<epoch>` from every `write_profiles` call (board
    // deploys, `profile init`) — the exact unbounded backup scheme this tool
    // exists to reclaim. Reclaiming only two of the three would leave the
    // user staring at the remaining pile forever.
    report.stale_backups = find_stale_backups(&registry)?;
    report.stale_backups.extend(find_stale_backups(&settings)?);
    report.stale_backups.extend(find_stale_backups(&cfg_path)?);
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
    } else if report.seeded_profiles {
        // Without --fix, profiles.json was never written, so nothing was ever
        // loaded to check scope against. Reporting "already consistent" here
        // would be the same anti-pattern C1 exists to kill: a diagnostic
        // claiming health it never established.
        println!("plugin scope: no profiles.json — nothing to check");
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
            println!("pruned {} stale backup(s):", report.pruned_backups);
        } else if fix {
            // Only --fix was given: --prune-backups is the missing ingredient.
            println!(
                "{} stale backup(s) found (pass --prune-backups to delete):",
                report.stale_backups.len()
            );
        } else {
            // Nothing ran at all without --fix, regardless of whether the user
            // already passed --prune-backups — name the ingredient that is
            // actually missing instead of repeating a flag they may have given.
            println!(
                "{} stale backup(s) found (run with --fix --prune-backups to delete):",
                report.stale_backups.len()
            );
        }
        for p in &report.stale_backups {
            println!("  {}", p.display());
        }
    }
    if !fix {
        println!("\n(run with --fix to apply)");
    }
}
