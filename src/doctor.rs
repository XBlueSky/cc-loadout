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

/// Where the launcher maintains its PATH symlink. MUST stay identical to that
/// script's `$LINK`. (Its `$DATA` counterpart is not duplicated here: `run`'s
/// caller already has that path as `data_root` from `resolve_env`, and takes
/// it as a parameter below.)
pub fn link_path(home: &Path) -> PathBuf {
    // A relative CC_LOADOUT_LINK_DIR must be ignored, for the same reason as
    // resolve_env()'s XDG_DATA_HOME handling in src/main.rs: the XDG Base
    // Directory spec treats a relative value here as invalid, and honouring
    // it anyway would resolve against whatever process's cwd happens to be
    // running `doctor`, the launcher, or the hook — three different
    // directories in practice. See scripts/launcher.sh's normalize_dir_var
    // for the shell-side twin (which also strips a trailing slash — moot
    // here, since PathBuf::join already suppresses a doubled separator).
    match std::env::var_os("CC_LOADOUT_LINK_DIR") {
        Some(v) if !v.is_empty() && Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home.join(".local").join("bin"),
    }
    .join("cc-loadout")
}

/// Where this run's backup will go. Never clobbers a previous convergence's
/// backup: if `cc-loadout.standalone.bak` is already occupied — a second
/// regular file landed at `link` after an earlier `--fix` already converged
/// one (a repeat `install.sh`, `cargo install --root`, or a hand-restored
/// wrapper) — `std::fs::rename` would otherwise silently replace it, losing
/// whatever the first convergence saved with no way back. The next free
/// `cc-loadout.standalone.bak.<n>` is used instead; the numeral lives in the
/// `.standalone.bak` stem, not after a `.bak.` of its own, so every candidate
/// still fails `find_stale_backups`'s `cc-loadout.bak.` prefix test and none
/// of them is ever swept by `--prune-backups`.
fn standalone_backup_path(link: &Path) -> PathBuf {
    // `symlink_metadata`, not `exists()`: `exists()` follows symlinks, so a
    // *dangling* `cc-loadout.standalone.bak` would read as free and the
    // following `rename` would silently replace it. No user data is lost — a
    // broken symlink holds none — but the whole point of this function is
    // never mistaking an occupied name for a free one, and `symlink_metadata`
    // is the probe that actually answers "is something here" rather than "is
    // something reachable through here", the same reason the rest of this
    // module uses it over `metadata`.
    let base = link.with_file_name("cc-loadout.standalone.bak");
    if std::fs::symlink_metadata(&base).is_err() {
        return base;
    }
    let mut n: u32 = 1;
    loop {
        let candidate = link.with_file_name(format!("cc-loadout.standalone.bak.{n}"));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        n += 1;
    }
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
/// lives under `data_root` is what stops `./target/release/cc-loadout doctor
/// --fix` from pointing the user's PATH at an uncommitted dev build, and stops
/// the step firing at all for someone who has no plugin installed. Taking the
/// paths explicitly also keeps this testable without mutating process-global
/// env vars.
fn converge_standalone(
    data_root: &Path,
    link: &Path,
    exe: &Path,
    fix: bool,
) -> Result<Option<StandaloneLink>> {
    // Canonicalize before comparing. `current_exe()` is fully resolved on
    // Linux (it's `readlink("/proc/self/exe")`), but `data_root` is built
    // from literal env vars, so a symlinked $HOME or `~/.local` (a dotfile
    // manager, home on a second volume) would make the unresolved comparison
    // false forever: `doctor --fix` would silently do nothing while
    // `hooks/hook.sh` kept printing the hint that sends users to the very
    // command that just no-opped. Canonicalization failing is not an error
    // here — a path that does not exist yet is the ordinary case, not a
    // problem — so it falls back to the literal path, exactly the comparison
    // this guard used before either side could be resolved.
    let exe_resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let data_resolved =
        std::fs::canonicalize(data_root).unwrap_or_else(|_| data_root.to_path_buf());
    if !exe_resolved.starts_with(&data_resolved) {
        return Ok(None);
    }
    // `CC_LOADOUT_LINK_DIR` pointing inside the data dir is misconfiguration,
    // not an installation to converge. Without this, `fix` below would rename
    // the running pinned binary out from under itself and then symlink the
    // name it just vacated right back onto it: a self-referencing dangling
    // link. The launcher's next download would heal it, but there is no
    // reason to let this step create that mess to begin with. Compared
    // against the literal `data_root`, not `data_resolved`: `link` is built
    // from the same un-resolved env vars `data_root` is, so comparing it
    // against the canonicalized form would reintroduce the exact
    // resolved-vs-literal mismatch the guard above exists to fix (and, on a
    // macOS tmp dir where `/var` is itself a symlink, fail this comparison
    // for every ordinary — non-misconfigured — install).
    //
    // `|| link == exe_resolved` closes the one case the literal prefix check
    // above cannot see: `CC_LOADOUT_LINK_DIR` set (or aliased through some
    // other symlink) directly to the *already-resolved* location of the
    // running binary, bypassing `data_root`'s own symlink entirely. `link`
    // itself is not canonicalized for this — that would reintroduce the same
    // mixed comparison the prefix check's comment warns about — this only
    // catches the case where the literal value the caller gave already *is*
    // the resolved path, which needs no further resolving to compare.
    if link.starts_with(data_root) || link == exe_resolved {
        return Ok(None);
    }
    // Rebase the resolved exe back onto the literal `data_root` before it is
    // ever written down anywhere. `scripts/launcher.sh`'s `reconcile_link`
    // only recognizes a link spelled literally as `$DATA/bin/<pin>/cc-loadout`
    // (`$DATA` being the SAME un-resolved env-var path `data_root` is) — if
    // `data_root` is reached through a symlink, `exe`'s already-resolved
    // spelling falls outside that pattern, and the launcher would treat the
    // link this step just wrote as "someone else's" and never repoint it on
    // the next version bump. The user would be silently stuck on the old
    // pinned binary, and once `gc_old_versions` reclaims its now-unpinned
    // directory the PATH entry goes fully dangling — with no hint printed
    // either, since the link is no longer the foreign kind `hooks/hook.sh`
    // looks for. `strip_prefix` cannot fail here: the guard above already
    // established `exe_resolved.starts_with(&data_resolved)`.
    let suffix = exe_resolved
        .strip_prefix(&data_resolved)
        .expect("checked exe_resolved.starts_with(&data_resolved) above");
    let exe_for_link = data_root.join(suffix);
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
    let backup = standalone_backup_path(link);
    if fix {
        std::fs::rename(link, &backup)
            .with_context(|| format!("backing up {} to {}", link.display(), backup.display()))?;
        if let Err(symlink_err) = std::os::unix::fs::symlink(&exe_for_link, link) {
            // The rename above already succeeded: `link` is now empty and the
            // user's original binary is sitting at `backup` with nothing on
            // PATH at all. Restore it rather than leaving the user with
            // neither a standalone binary nor a working symlink — best
            // effort, and if the rollback itself fails, say so instead of
            // hiding it. Either way the error must name `backup`: it is the
            // one fact that lets the user recover by hand.
            return match std::fs::rename(&backup, link) {
                Ok(()) => Err(symlink_err).with_context(|| {
                    format!(
                        "linking {} -> {}: restored the original from {}",
                        link.display(),
                        exe_for_link.display(),
                        backup.display()
                    )
                }),
                Err(rollback_err) => Err(symlink_err).with_context(|| {
                    format!(
                        "linking {} -> {}: the original is saved at {} but restoring it also failed: {rollback_err}",
                        link.display(),
                        exe_for_link.display(),
                        backup.display()
                    )
                }),
            };
        }
    }
    Ok(Some(StandaloneLink {
        path: link.to_path_buf(),
        backup,
        target: exe_for_link,
        converged: fix,
    }))
}

pub fn run(
    home: &Path,
    config_override: Option<&Path>,
    data_root: &Path,
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
        report.standalone_link = converge_standalone(data_root, &link_path(home), exe, fix)?;
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
    /// it just saved — including the numbered fallback name a second
    /// convergence picks when the first backup is still there.
    #[test]
    fn the_standalone_backup_is_not_swept_by_prune_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("bin/cc-loadout");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(link.with_file_name("cc-loadout.standalone.bak"), b"saved").unwrap();
        std::fs::write(
            link.with_file_name("cc-loadout.standalone.bak.1"),
            b"saved2",
        )
        .unwrap();
        std::fs::write(link.with_file_name("cc-loadout.bak.123"), b"stale").unwrap();

        let found = find_stale_backups(&link).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file_name().unwrap(), "cc-loadout.bak.123");
    }

    /// `exists()` follows symlinks, so a *dangling* `cc-loadout.standalone.bak`
    /// would read as free and the following `rename` would silently replace
    /// it. `standalone_backup_path` must use `symlink_metadata` instead, which
    /// sees the dangling entry for what it is: something already there.
    #[test]
    fn standalone_backup_path_skips_a_dangling_symlink_by_that_name() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("bin/cc-loadout");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(
            tmp.path().join("nowhere"),
            link.with_file_name("cc-loadout.standalone.bak"),
        )
        .unwrap();

        let picked = standalone_backup_path(&link);
        assert_eq!(
            picked.file_name().unwrap(),
            "cc-loadout.standalone.bak.1",
            "a dangling symlink must not be mistaken for a free slot"
        );
    }

    /// A second regular file landing at `link` after an earlier `--fix`
    /// already converged one (a repeat `install.sh`, `cargo install --root`,
    /// a hand-restored wrapper) must not clobber the first backup — that
    /// backup may be the user's only remaining copy of a hand-written wrapper
    /// that a reinstall cannot regenerate.
    #[test]
    fn converge_does_not_clobber_an_existing_standalone_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        converge_standalone(&data, &link, &exe, true).unwrap();
        let first_backup = link.with_file_name("cc-loadout.standalone.bak");
        assert_eq!(
            std::fs::read(&first_backup).unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );

        // Drop the symlink convergence 1 left behind and put a fresh regular
        // file back at `link`, exactly as a repeat install would.
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"second standalone file").unwrap();

        let out = converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .expect("the second regular file must be reported too");
        assert_ne!(
            out.backup, first_backup,
            "the second convergence must not reuse the first backup's name"
        );
        assert_eq!(
            std::fs::read(&first_backup).unwrap(),
            b"#!/bin/sh\nexit 0\n",
            "the first backup must survive the second convergence untouched"
        );
        assert_eq!(
            std::fs::read(&out.backup).unwrap(),
            b"second standalone file"
        );
    }

    /// If `rename` succeeds but the following `symlink` fails, the user must
    /// not be left with neither a working `cc-loadout` on PATH nor a
    /// recoverable original — and the error must name the one path that lets
    /// them recover by hand. A symlink target long enough to blow past every
    /// filesystem's symlink-content limit forces exactly that ordering
    /// (`rename` doesn't touch this string at all; `symlink` fails writing
    /// it) without needing any fault-injection machinery.
    #[test]
    fn converge_rolls_back_and_names_the_backup_when_symlinking_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join(format!("bin/9.9.9/{}", "a".repeat(8192)));
        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);
        let original = std::fs::read(&link).unwrap();

        let err = converge_standalone(&data, &link, &exe, true).unwrap_err();
        // Unique to the rollback-succeeded branch: `rename`'s own "backing up
        // ... to ..." context also mentions `cc-loadout.standalone.bak`, so
        // asserting on that substring alone would stay green even if a future
        // refactor made `rename` the failing call instead of `symlink` — this
        // wording only appears once the rollback itself has run.
        assert!(
            format!("{err:#}").contains("restored the original from"),
            "the recovery path must be named via the rollback branch specifically: {err:#}"
        );
        assert_eq!(
            std::fs::read(&link).unwrap(),
            original,
            "a failed convergence must restore the original file"
        );
        assert!(
            !link.with_file_name("cc-loadout.standalone.bak").exists(),
            "the backup must not be left behind once rolled back"
        );
    }

    /// On Linux `current_exe()` is fully resolved (`readlink` of
    /// `/proc/self/exe`) while `data_root` is built from literal env vars, so
    /// a symlinked `$HOME` or `~/.local` (a dotfile manager, home on a second
    /// volume) must still match once both sides are canonicalized. This is
    /// the one case this repo's own macOS dev/test environment cannot catch
    /// by accident, because macOS's `current_exe()` does not canonicalize.
    #[test]
    fn converge_matches_when_data_root_is_reached_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let real_data = tmp.path().join("real/data/cc-loadout");
        let exe = real_data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);

        // `data_root` as literally constructed from env vars: a symlink to
        // the directory that actually holds the binary.
        let data_via_symlink = tmp.path().join("home/.local/share/cc-loadout");
        std::fs::create_dir_all(data_via_symlink.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_data, &data_via_symlink).unwrap();

        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        let out = converge_standalone(&data_via_symlink, &link, &exe, true)
            .unwrap()
            .expect("canonicalizing both sides must still match");
        assert!(out.converged);
    }

    /// The launcher's `reconcile_link` (`scripts/launcher.sh:111`) only
    /// repoints a link literally spelled `$DATA/bin/<pin>/cc-loadout` — the
    /// SAME un-resolved `$DATA` `data_root` is built from. If the symlink this
    /// step writes were spelled with the *resolved* exe path instead, the
    /// launcher would see a foreign link on the next version bump, never
    /// repoint it, and `gc_old_versions` would eventually delete the pinned
    /// binary out from under a dangling PATH entry. Asserting only that the
    /// link *resolves* to the right file (as the sibling test above does)
    /// would pass even with that defect present — this pins the spelling.
    #[test]
    fn converge_spells_the_link_target_using_the_literal_data_root() {
        let tmp = tempfile::tempdir().unwrap();
        let real_data = tmp.path().join("real/data/cc-loadout");
        let exe = real_data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);

        let data_via_symlink = tmp.path().join("home/.local/share/cc-loadout");
        std::fs::create_dir_all(data_via_symlink.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_data, &data_via_symlink).unwrap();

        let link = tmp.path().join("bin/cc-loadout");
        touch_exec(&link);

        let out = converge_standalone(&data_via_symlink, &link, &exe, true)
            .unwrap()
            .expect("a regular file must be reported");

        // The shape `reconcile_link` recognizes: literally under `data_root`
        // (the symlinked spelling), never under `real_data` (the resolved
        // one), even though both name the same file on disk.
        let expected_target = data_via_symlink.join("bin/9.9.9/cc-loadout");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            expected_target,
            "the link must be spelled under the literal data_root, not the resolved one"
        );
        assert_eq!(out.target, expected_target);
    }

    /// The literal prefix check above cannot see `CC_LOADOUT_LINK_DIR`
    /// aliased directly onto the *already-resolved* location of the running
    /// binary while `data_root` itself is still spelled through its own
    /// symlink — `link` and `data_root` no longer share a literal prefix at
    /// all. `link == exe_resolved` closes exactly that gap.
    #[test]
    fn converge_is_a_noop_when_link_is_the_resolved_exe_reached_by_a_different_alias() {
        let tmp = tempfile::tempdir().unwrap();
        // Canonicalize the tmp root itself first, so nothing below is
        // affected by an ambient symlink in the test environment (e.g.
        // macOS's `/var` -> `/private/var`) — the ONLY deliberate symlink
        // here is `data_via_symlink`.
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let real_data = root.join("real/data/cc-loadout");
        let exe = real_data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);

        let data_via_symlink = root.join("home/.local/share/cc-loadout");
        std::fs::create_dir_all(data_via_symlink.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real_data, &data_via_symlink).unwrap();

        // CC_LOADOUT_LINK_DIR set directly to the resolved location: bypasses
        // `data_root`'s own symlink entirely, so it shares no literal prefix
        // with `data_root` at all, and lands exactly on the running binary.
        let link = exe.clone();

        assert!(converge_standalone(&data_via_symlink, &link, &exe, true)
            .unwrap()
            .is_none());
    }

    /// `CC_LOADOUT_LINK_DIR` pointing inside the data dir is misconfiguration:
    /// without this guard, `--fix` would rename the running pinned binary out
    /// from under itself and symlink the vacated name right back onto it.
    #[test]
    fn converge_is_a_noop_when_link_is_inside_the_data_root() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data/cc-loadout");
        let exe = data.join("bin/9.9.9/cc-loadout");
        touch_exec(&exe);
        let link = data.join("bin/cc-loadout");
        touch_exec(&link);

        assert!(converge_standalone(&data, &link, &exe, true)
            .unwrap()
            .is_none());
    }
}
