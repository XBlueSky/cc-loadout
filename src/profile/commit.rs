use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::profile::config::Profiles;
use crate::profile::plugins::managed_keys;
use crate::profile::{apply, author, detect, discover, scan_cache, signal_detect};

pub struct CommitReport {
    pub profiles_path: PathBuf,
    #[allow(dead_code)]
    pub global_disabled: usize,
    #[allow(dead_code)]
    pub global_kept: usize,
    pub repos_applied: usize,
    /// Number of written repos whose FRESH write-time `detect_profiles` (set
    /// of matched profile names) disagreed with `expected` — the preview the
    /// Apply screen showed before Enter was pressed. The preview is built
    /// from the index and can go stale between scan and write; this is the
    /// after-the-fact honesty check on that gap.
    pub diverged: usize,
    /// Each written repo's freshly re-detected `RepoSignal` (same values just
    /// merged into the on-disk scan cache above). The caller (the TUI's
    /// `Action::Commit` handler) folds these into the in-memory inventory too
    /// — via `IndexOutcome`/`accept_index`, same as `IndexAtoms` — so
    /// reopening Apply right after a commit reflects fresh truth without a
    /// restart or an explicit rescan.
    pub fresh_signals: Vec<discover::RepoSignal>,
}

/// Write profiles.json (+ backup), sync global settings.json (Model C), then
/// apply each selected repo's settings.local.json. One commit, in this order.
/// `expected` is the Apply preview's matched-set per repo (order-independent
/// lookup by path) — used only to compute `CommitReport.diverged`, never to
/// decide what gets written (the write always uses the fresh `detect_profiles`
/// call below, exactly as before this field existed).
pub fn commit(
    cfg_path: &Path,
    settings_path: &Path,
    data_root: &Path,
    working: &Profiles,
    repos: &[PathBuf],
    expected: &[(PathBuf, Vec<String>)],
    now_epoch: i64,
) -> Result<CommitReport> {
    author::write_profiles(cfg_path, working, now_epoch)?;

    let (_before, after) = apply::apply_global(settings_path, working)?;
    let (mut kept, mut disabled) = (0usize, 0usize);
    if let Value::Object(map) = &after {
        for key in managed_keys(working) {
            match map.get(&key).and_then(Value::as_bool) {
                Some(true) => kept += 1,
                _ => disabled += 1,
            }
        }
    }

    let expected_map: BTreeMap<&PathBuf, &Vec<String>> =
        expected.iter().map(|(p, m)| (p, m)).collect();

    let mut repos_applied = 0usize;
    let mut diverged = 0usize;
    let mut fresh_signals: Vec<discover::RepoSignal> = Vec::new();
    let vocab = if repos.is_empty() {
        None
    } else {
        Some(signal_detect::vocabulary(working))
    };
    for repo in repos {
        let matched = detect::detect_profiles(repo, working);
        apply::apply(repo, working, &matched)?;
        repos_applied += 1;

        if let Some(exp) = expected_map.get(repo) {
            let got: std::collections::BTreeSet<&String> = matched.iter().collect();
            let want: std::collections::BTreeSet<&String> = exp.iter().collect();
            if got != want {
                diverged += 1;
            }
        }

        if let Some(vocab) = &vocab {
            let (sig, _exhausted) =
                discover::signals_for_repo(repo, vocab, detect::GLOB_WALK_BUDGET);
            fresh_signals.push(sig);
        }
    }

    // Best-effort load-merge-save into the scan cache, mirroring the
    // IndexAtoms job: a killed TUI at worst loses this in-flight refresh,
    // never corrupts the cache. Unlike IndexAtoms (which only patches
    // specific atoms), each written repo's ENTIRE cache entry is replaced
    // with its freshly re-detected signal — we just recomputed the whole
    // thing above, so a partial atom-merge would leave other fields stale.
    if !fresh_signals.is_empty() {
        if let Some(mut cache) = scan_cache::load(data_root) {
            for fresh in &fresh_signals {
                match cache.repos.iter_mut().find(|r| r.path == fresh.path) {
                    Some(slot) => *slot = fresh.clone(),
                    None => cache.repos.push(fresh.clone()),
                }
            }
            let _ = scan_cache::save(data_root, &cache);
        }
    }

    Ok(CommitReport {
        profiles_path: cfg_path.to_path_buf(),
        global_disabled: disabled,
        global_kept: kept,
        repos_applied,
        diverged,
        fresh_signals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profiles;

    fn working() -> Profiles {
        serde_json::from_str(
            r#"{"scan_roots":[],"universal":["serena@x"],
                "profiles":{"rust":{"plugins":["ra@x"],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        ).unwrap()
    }

    #[test]
    fn commit_writes_profiles_global_and_selected_repos() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let data_root = home.path();
        // a rust repo to apply to
        let repo = home.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let rep = commit(
            &cfg_path,
            &settings,
            data_root,
            &working(),
            std::slice::from_ref(&repo),
            &[(repo.clone(), vec!["rust".to_string()])],
            100,
        )
        .unwrap();

        // profiles.json written
        let reloaded = crate::profile::config::load(&cfg_path).unwrap();
        assert!(reloaded.profiles.contains_key("rust"));
        // global: serena kept (universal), ra disabled
        let g: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(g["enabledPlugins"]["serena@x"], serde_json::json!(true));
        assert_eq!(g["enabledPlugins"]["ra@x"], serde_json::json!(false));
        assert_eq!(rep.global_kept, 1);
        assert_eq!(rep.global_disabled, 1);
        // repo got rust's plugins enabled locally
        let local: serde_json::Value = serde_json::from_slice(
            &std::fs::read(repo.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(local["enabledPlugins"]["ra@x"], serde_json::json!(true));
        assert_eq!(rep.repos_applied, 1);
        assert_eq!(
            rep.diverged, 0,
            "expected matches fresh detect => no divergence"
        );
    }

    #[test]
    fn commit_with_zero_repos_still_writes_profiles_and_global() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let data_root = home.path();
        let rep = commit(&cfg_path, &settings, data_root, &working(), &[], &[], 100).unwrap();
        assert!(cfg_path.exists());
        assert!(settings.exists());
        assert_eq!(rep.repos_applied, 0);
        assert_eq!(rep.diverged, 0);
    }

    #[test]
    fn commit_reports_diverged_when_fresh_detect_disagrees_with_preview() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let data_root = home.path();
        let repo = home.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        // Preview claimed "no match", but fresh disk truth at write time is
        // "rust" (Cargo.toml appeared between preview and write) — a set
        // inequality, so this must count as diverged.
        let rep = commit(
            &cfg_path,
            &settings,
            data_root,
            &working(),
            std::slice::from_ref(&repo),
            &[(repo.clone(), vec![])],
            100,
        )
        .unwrap();

        assert_eq!(rep.diverged, 1);
    }

    #[test]
    fn commit_report_carries_fresh_signals_for_written_repos_only() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let data_root = home.path();
        let repo = home.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
        let canon = std::fs::canonicalize(&repo).unwrap();

        let rep = commit(
            &cfg_path,
            &settings,
            data_root,
            &working(),
            std::slice::from_ref(&repo),
            &[(repo.clone(), vec!["rust".to_string()])],
            100,
        )
        .unwrap();

        assert_eq!(
            rep.fresh_signals.len(),
            1,
            "one written repo => one fresh signal"
        );
        let sig = &rep.fresh_signals[0];
        assert_eq!(sig.path, canon.display().to_string());
        assert_eq!(
            sig.rule_hits.get("file:Cargo.toml"),
            Some(&true),
            "fresh signal must carry the just-recomputed rule_hits: {sig:?}"
        );
    }

    #[test]
    fn commit_refreshes_scan_cache_only_for_written_repos() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let data_root = home.path();

        let repo_a_raw = home.path().join("a");
        std::fs::create_dir_all(&repo_a_raw).unwrap();
        let repo_a = std::fs::canonicalize(&repo_a_raw).unwrap();
        let repo_b_raw = home.path().join("b");
        std::fs::create_dir_all(&repo_b_raw).unwrap();
        let repo_b = std::fs::canonicalize(&repo_b_raw).unwrap();

        fn stub_signal(path: &std::path::Path) -> crate::profile::discover::RepoSignal {
            crate::profile::discover::RepoSignal {
                path: path.display().to_string(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            }
        }

        // Pre-seed a stale cache: neither repo has Cargo.toml indexed yet.
        let stale = crate::profile::scan_cache::ScanCache {
            version: crate::profile::scan_cache::SCAN_CACHE_VERSION,
            roots: vec![home.path().display().to_string()],
            repos: vec![stub_signal(&repo_a), stub_signal(&repo_b)],
            uncovered: Some(vec![]),
            scanned_at: 1,
        };
        crate::profile::scan_cache::save(data_root, &stale).unwrap();

        // Cargo.toml appears on disk for repo_a only, AFTER the stale cache
        // was written — repo_b's on-disk state never changes.
        std::fs::write(repo_a.join("Cargo.toml"), "[package]").unwrap();

        commit(
            &cfg_path,
            &settings,
            data_root,
            &working(),
            std::slice::from_ref(&repo_a),
            &[(repo_a.clone(), vec!["rust".to_string()])],
            100,
        )
        .unwrap();

        let cache = crate::profile::scan_cache::load(data_root).unwrap();
        let entry_a = cache
            .repos
            .iter()
            .find(|r| r.path == repo_a.display().to_string())
            .expect("repo_a must still be in the cache");
        assert!(
            entry_a.marker_files.contains(&"Cargo.toml".to_string()),
            "written repo's cache entry must be refreshed with fresh truth: {entry_a:?}"
        );
        let entry_b = cache
            .repos
            .iter()
            .find(|r| r.path == repo_b.display().to_string())
            .expect("repo_b must still be in the cache");
        assert!(
            entry_b.marker_files.is_empty(),
            "untouched repo's cache entry must NOT be refreshed: {entry_b:?}"
        );
    }
}
