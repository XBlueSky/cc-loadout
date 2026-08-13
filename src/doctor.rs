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
    pub standalone_link: Option<StandaloneLink>,
}

/// A standalone `cc-loadout` on PATH that this run converged onto the
/// plugin-managed binary (or, without `--fix`, would have).
#[derive(Debug)]
pub struct StandaloneLink {
    pub path: PathBuf,
    pub backup: PathBuf,
    pub target: PathBuf,
    pub converged: bool,
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

/// Where `scripts/launcher.sh` keeps its pinned binaries. This MUST stay
/// identical to that script's `$DATA` — deliberately not `$CLAUDE_PLUGIN_DATA`,
/// which the launcher explains at length.
pub fn data_dir(home: &Path) -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".local").join("share"),
    }
    .join("cc-loadout")
}

/// Where the launcher maintains its PATH symlink. MUST stay identical to that
/// script's `$LINK`.
pub fn link_path(home: &Path) -> PathBuf {
    match std::env::var_os("CC_LOADOUT_LINK_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".local").join("bin"),
    }
    .join("cc-loadout")
}

/// Converge a standalone install at the launcher's link path onto the
/// plugin-managed binary.
///
/// The launcher deliberately refuses to touch a regular file there — moving a
/// user's binary is not something a session-start hook should do unasked — so
/// this is the deliberate, user-invoked half of that split. `hooks/hook.sh`
/// prints the command that gets here.
///
/// `exe` is this process's own executable, and the guard below is why it is a
/// parameter rather than a `current_exe()` call inside: acting only when `exe`
/// lives under `data_dir` is what stops `./target/release/cc-loadout doctor
/// --fix` from pointing the user's PATH at an uncommitted dev build, and stops
/// the step firing at all for someone who has no plugin installed. Taking the
/// paths explicitly also keeps this testable without mutating process-global
/// env vars.
fn converge_standalone(
    data_dir: &Path,
    link: &Path,
    exe: &Path,
    fix: bool,
) -> Result<Option<StandaloneLink>> {
    if !exe.starts_with(data_dir) {
        return Ok(None);
    }
    // `symlink_metadata`, not `metadata`: the latter follows the link, so the
    // launcher's own symlink would present as the regular file it points at and
    // this step would "converge" a link that is already correct.
    let md = match std::fs::symlink_metadata(link) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", link.display())),
    };
    if !md.file_type().is_file() {
        return Ok(None);
    }
    // A fixed suffix, never `.bak.<epoch>`: `find_stale_backups` builds the
    // prefix `cc-loadout.bak.` and sweeps all-digit suffixes, so an epoch name
    // here would be deleted by `--fix --prune-backups` — the tool erasing the
    // binary it had just saved.
    let backup = link.with_file_name("cc-loadout.standalone.bak");
    if fix {
        std::fs::rename(link, &backup)
            .with_context(|| format!("backing up {} to {}", link.display(), backup.display()))?;
        std::os::unix::fs::symlink(exe, link)
            .with_context(|| format!("linking {} -> {}", link.display(), exe.display()))?;
    }
    Ok(Some(StandaloneLink {
        path: link.to_path_buf(),
        backup,
        target: exe.to_path_buf(),
        converged: fix,
    }))
}

pub fn run(
    home: &Path,
    config_override: Option<&Path>,
    exe: Option<&Path>,
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

    // Deliberately propagates rather than being swallowed: unlike the hook
    // paths, `doctor` is the one place a diagnostic must not report health it
    // never established. A `None` exe (current_exe() failed) is simply no
    // information, so the step is skipped.
    if let Some(exe) = exe {
        report.standalone_link = converge_standalone(&data_dir(home), &link_path(home), exe, fix)?;
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
    if let Some(sl) = &report.standalone_link {
        if sl.converged {
            println!("converged the standalone install on PATH:");
            println!("  saved  {} -> {}", sl.path.display(), sl.backup.display());
            println!("  linked {} -> {}", sl.path.display(), sl.target.display());
        } else {
            println!(
                "standalone install at {} (--fix would save it to {} and link the plugin's {})",
                sl.path.display(),
                sl.backup.display(),
                sl.target.display()
            );
        }
    }
    if !fix {
        println!("\n(run with --fix to apply)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_exec(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"#!/bin/sh\nexit 0\n").unwrap();
    }

    #[test]
    fn converge_backs_up_a_regular_file_and_links_the_pinned_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        let out = converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .expect("a regular file must be reported");
        assert!(out.converged);
        assert_eq!(out.backup.file_name().unwrap(), "cc-loadout.standalone.bak");
        assert!(out.backup.exists(), "the original must be kept");
        assert_eq!(std::fs::read_link(&link).unwrap(), exe);
    }

    #[test]
    fn converge_reports_without_fix_but_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        let out = converge_standalone(&data, &link, &exe, false)
            .unwrap()
            .unwrap();
        assert!(!out.converged);
        assert!(!out.backup.exists(), "without --fix nothing may be written");
        assert!(
            std::fs::read_link(&link).is_err(),
            "must still be a regular file"
        );
    }

    /// The guard that stops `./target/release/cc-loadout doctor --fix` from
    /// pointing the user's PATH at an uncommitted dev build — and stops this
    /// step firing at all for someone with no plugin installed.
    #[test]
    fn converge_is_a_noop_when_the_exe_is_outside_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        std::fs::create_dir_all(&data).unwrap();
        let exe = tmp.path().join("target/release/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        assert!(converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .is_none());
        assert!(
            std::fs::read_link(&link).is_err(),
            "the file must be untouched"
        );
    }

    #[test]
    fn converge_leaves_an_existing_symlink_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&exe, &link).unwrap();

        assert!(converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .is_none());
    }

    #[test]
    fn converge_is_a_noop_when_nothing_is_installed_at_the_link_path() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");

        assert!(converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .is_none());
    }

    /// `--fix --prune-backups` sweeps `cc-loadout.bak.<digits>`. The backup this
    /// step writes must not be swept, or the tool would delete the very binary
    /// it just saved.
    #[test]
    fn the_standalone_backup_is_not_swept_by_prune_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("bin/cc-loadout");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(link.with_file_name("cc-loadout.standalone.bak"), b"saved").unwrap();
        std::fs::write(link.with_file_name("cc-loadout.bak.123"), b"stale").unwrap();

        let found = find_stale_backups(&link).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_name().unwrap(), "cc-loadout.bak.123");
    }
}
