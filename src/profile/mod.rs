//! Profile subsystem: per-repo plugin profile detection and settings.local.json apply.

pub mod ai;
pub mod apply;
pub mod author;
pub mod commit;
pub mod config;
pub mod detect;
pub mod discover;
pub mod draft;
pub mod drift;
pub mod init;
pub mod json;
pub mod on_demand;
pub mod plugins;
pub mod registry;
pub mod scan;
pub mod scan_cache;
pub mod signal_detect;

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::profile::config::Profiles;

#[derive(Clone, Copy)]
pub enum Action {
    Detect,
    Apply,
    Status,
}

/// Run a profile action on a single path or, with `all`, on every git repo under
/// `cfg.scan_roots`. `json` switches all three actions to the machine-readable
/// `{schema_version, repos:[…]}` envelope. `dry_run` only affects `Apply`: it
/// computes the same diff without writing, so callers can audit which repos are
/// out of sync (ignored for `Detect`/`Status`, which never write).
pub fn run(
    cfg: &Profiles,
    action: Action,
    path: Option<PathBuf>,
    all: bool,
    json: bool,
    dry_run: bool,
) -> Result<()> {
    let repos = repos_to_process(cfg, path, all)?;

    if json {
        match action {
            Action::Detect => {
                let out: Vec<json::DetectRepoJson> = repos
                    .iter()
                    .map(|r| json::detect_repo_json(r, cfg))
                    .collect();
                crate::json::emit(&json::ReposJson { repos: out })?;
            }
            Action::Apply => {
                let mut out = Vec::with_capacity(repos.len());
                for r in &repos {
                    out.push(json::apply_repo_json(r, cfg, dry_run)?);
                }
                crate::json::emit(&json::ReposJson { repos: out })?;
            }
            Action::Status => {
                let mut out = Vec::with_capacity(repos.len());
                for r in &repos {
                    out.push(json::status_repo_json(r)?);
                }
                crate::json::emit(&json::ReposJson { repos: out })?;
            }
        }
        return Ok(());
    }

    // `apply --all` over hundreds of repos: printing every unchanged repo drowns
    // the signal, so route it through a helper that shows only the repos that
    // (would) change plus an audit summary.
    if all && matches!(action, Action::Apply) {
        return run_apply_all(cfg, &repos, dry_run);
    }

    if all {
        for repo in &repos {
            println!("--- {} ---", repo.display());
            one(action, repo, cfg, dry_run)?;
        }
        println!("\nSummary: {} repos visited", repos.len());
    } else {
        one(action, &repos[0], cfg, dry_run)?;
    }
    Ok(())
}

/// Human-readable `apply --all` (and its `--dry-run`): apply (or preview) every
/// repo but print only the ones that (would) change, then a one-line audit
/// summary. This is the scalable answer to "which repos still need applying?"
/// across a large scan root.
fn run_apply_all(cfg: &Profiles, repos: &[PathBuf], dry_run: bool) -> Result<()> {
    let verb = if dry_run { "would change" } else { "changed" };
    let mut changed = 0usize;
    let mut uncovered = 0usize;

    for repo in repos {
        let matched = detect::detect_profiles(repo, cfg);
        if matched.is_empty() {
            uncovered += 1;
        }
        let (before, after) = if dry_run {
            apply::preview(repo, cfg, &matched)?
        } else {
            apply::apply(repo, cfg, &matched)?
        };
        if before != after {
            changed += 1;
            let label = if matched.is_empty() {
                "(none)".to_string()
            } else {
                matched.join(" ")
            };
            println!("--- {} ---", repo.display());
            println!("Profiles: {label}");
            print_diff(&before, &after);
        }
    }

    println!(
        "\nSummary: {changed} of {} repos {verb}; {uncovered} match no profile.",
        repos.len()
    );
    Ok(())
}

/// The repos a `run` call operates on: one resolved cwd, or every git repo under
/// `scan_roots` when `all`.
fn repos_to_process(cfg: &Profiles, path: Option<PathBuf>, all: bool) -> Result<Vec<PathBuf>> {
    if all {
        let mut repos = Vec::new();
        for root in &cfg.scan_roots {
            let root = Path::new(root);
            if !root.is_dir() {
                continue;
            }
            repos.extend(scan::find_git_repos(root, 6));
        }
        Ok(repos)
    } else {
        let cwd = match path {
            Some(p) => std::fs::canonicalize(&p).unwrap_or(p),
            None => std::env::current_dir()?,
        };
        Ok(vec![cwd])
    }
}

fn one(action: Action, repo: &Path, cfg: &Profiles, dry_run: bool) -> Result<()> {
    match action {
        Action::Detect => {
            let explained = detect::detect_profiles_explained(repo, cfg);
            let matched: Vec<String> = explained.iter().map(|(n, _)| n.clone()).collect();
            println!("Repo: {}", repo.display());
            if matched.is_empty() {
                println!("Profiles: (none — default)");
            } else {
                println!("Profiles: {}", matched.join(" "));
                println!("Signals:");
                for (name, r) in &explained {
                    match &r.value {
                        Some(v) => println!("  {name}  <- {} \"{}\"", r.rule, v),
                        None => println!("  {name}  <- {}", r.rule),
                    }
                }
            }
            println!("Resulting enabled plugins:");
            for p in plugins::desired_plugins(cfg, &matched) {
                println!("  {p}");
            }
        }
        Action::Apply => {
            let matched = detect::detect_profiles(repo, cfg);
            let (before, after) = if dry_run {
                apply::preview(repo, cfg, &matched)?
            } else {
                apply::apply(repo, cfg, &matched)?
            };
            println!("Repo: {}", repo.display());
            let label = if matched.is_empty() {
                "(none)".to_string()
            } else {
                matched.join(" ")
            };
            println!("Profiles: {label}");
            if before == after {
                println!("  no changes");
            } else {
                let heading = if dry_run {
                    "Would change enabledPlugins:"
                } else {
                    "Changed enabledPlugins:"
                };
                println!("{heading}");
                print_diff(&before, &after);
            }
        }
        Action::Status => {
            println!("Repo: {}", repo.display());
            match apply::current_enabled(repo)? {
                None => println!("  no settings.local.json (or no enabledPlugins)"),
                Some(v) => {
                    if let Some(obj) = v.as_object() {
                        let mut keys: Vec<&String> = obj.keys().collect();
                        keys.sort();
                        for k in keys {
                            println!("  {}: {}", k, obj[k]);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Write `.claude/profile` in `cwd` with validated profile names.
pub fn force(cwd: &Path, names: &[String], cfg: &Profiles) -> Result<()> {
    if names.is_empty() {
        bail!("force requires at least one profile name");
    }
    for n in names {
        if !cfg.profiles.contains_key(n) {
            let known: Vec<&str> = cfg.profiles.keys().map(String::as_str).collect();
            bail!("unknown profile '{n}'. Known: {}", known.join(" "));
        }
    }
    let dir = cwd.join(".claude");
    std::fs::create_dir_all(&dir)?;
    let body = format!("{}\n", names.join("\n"));
    crate::util::atomicfile::write_atomic(&dir.join("profile"), body.as_bytes(), 0o644)?;
    println!("Wrote {}:", dir.join("profile").display());
    for n in names {
        println!("  {n}");
    }
    Ok(())
}

/// Print the discovery inventory as pretty JSON (`json = true`) or a human table.
pub fn inventory(
    cfg: &Profiles,
    json: bool,
    root: Option<PathBuf>,
    home: &Path,
    config_override: Option<&Path>,
) -> Result<()> {
    let registry = discover::resolve_registry_path(home, config_override);
    if !registry.exists() {
        eprintln!(
            "note: plugin registry not found at {} — showing 0 plugins",
            registry.display()
        );
    }
    let roots: Vec<String> = match &root {
        Some(r) => vec![r.display().to_string()],
        None => cfg.scan_roots.clone(),
    };
    let inv = discover::build_inventory(&registry, &roots, 6);
    if json {
        crate::json::emit(&inv)?;
        return Ok(());
    }
    println!("Installed plugins ({}):", inv.plugins.len());
    for p in &inv.plugins {
        println!("  {} [{}]", p.key, p.scopes.join(","));
    }
    println!("\nSuggested profiles ({}):", inv.suggested_profiles.len());
    for s in &inv.suggested_profiles {
        println!("  {} — {} repo(s)", s.name, s.repos.len());
    }
    println!("\nRepos scanned: {}", inv.repos.len());
    Ok(())
}

/// Print keys whose enabled state differs between `before` and `after`.
fn print_diff(before: &serde_json::Value, after: &serde_json::Value) {
    let empty = serde_json::Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    let mut keys: std::collections::BTreeSet<&String> = a.keys().collect();
    keys.extend(b.keys());
    for k in keys {
        let bv = b.get(k);
        let av = a.get(k);
        if bv != av {
            println!(
                "  {k}: {} -> {}",
                bv.map(|v| v.to_string())
                    .unwrap_or_else(|| "(unset)".into()),
                av.map(|v| v.to_string())
                    .unwrap_or_else(|| "(unset)".into())
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_rejects_unknown_profile() {
        let cfg: Profiles =
            serde_json::from_str(r#"{"profiles":{"backend":{"plugins":[],"detect":{}}}}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = force(dir.path(), &["nope".to_string()], &cfg).unwrap_err();
        assert!(err.to_string().contains("unknown profile"));
    }

    #[test]
    fn force_writes_profile_file() {
        let cfg: Profiles =
            serde_json::from_str(r#"{"profiles":{"backend":{"plugins":[],"detect":{}}}}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        force(dir.path(), &["backend".to_string()], &cfg).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".claude").join("profile")).unwrap();
        assert_eq!(body, "backend\n");
    }
}
