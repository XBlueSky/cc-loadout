//! Persist the last repo scan so the Profile view shows meaningful counts on
//! reopen without re-walking the filesystem at startup (Stage B keeps startup
//! walk-free). Derived data — lives in the app-state dir, not in profiles.json.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::profile::discover::RepoSignal;
use crate::util::atomicfile;

/// Cache format version; incremented when schema changes (fields added/removed).
pub const SCAN_CACHE_VERSION: u32 = 2;

/// The cached result of one `s`/Rescan: which roots were walked, what repos were
/// found, and when (epoch seconds). Keyed on `roots` — a cache whose roots no
/// longer match the current scan roots is ignored on load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanCache {
    #[serde(default)]
    pub version: u32,
    pub roots: Vec<String>,
    pub repos: Vec<RepoSignal>,
    /// Repo paths that matched no profile as of this scan — the Profile board's
    /// "⚠ N repos match nothing" drift. Cached so the board can show it on
    /// reopen without re-running detection (a per-repo filesystem walk) at
    /// startup. `None` marks a cache written before uncovered was tracked (or
    /// otherwise never computed): startup treats that as "unknown" and kicks a
    /// one-time background backfill, whereas `Some(vec![])` means "computed, and
    /// nothing is uncovered" — which must NOT trigger a re-walk every launch.
    /// `#[serde(default)]` makes a legacy cache deserialize to `None`.
    #[serde(default)]
    pub uncovered: Option<Vec<String>>,
    pub scanned_at: i64,
}

/// `{data_root}/scan-cache.json`.
pub fn cache_path(data_root: &Path) -> PathBuf {
    data_root.join("scan-cache.json")
}

/// Read the cache, or `None` when absent or unreadable/corrupt (best effort — a
/// bad cache must never block startup).
pub fn load(data_root: &Path) -> Option<ScanCache> {
    let bytes = std::fs::read(cache_path(data_root)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write the cache atomically. Best-effort caller: a failed cache write must not
/// fail the scan it came from.
pub fn save(data_root: &Path, cache: &ScanCache) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(cache)?;
    atomicfile::write_atomic(&cache_path(data_root), &bytes, 0o644)
        .with_context(|| format!("writing {}", cache_path(data_root).display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(path: &str) -> RepoSignal {
        RepoSignal {
            path: path.into(),
            marker_files: vec!["Cargo.toml".into()],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec!["rs".into()],
            rule_hits: Default::default(),
            override_names: None,
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache {
            version: SCAN_CACHE_VERSION,
            roots: vec!["/workspace".into()],
            repos: vec![repo("/workspace/a"), repo("/workspace/b")],
            uncovered: Some(vec!["/workspace/b".into()]),
            scanned_at: 1_700_000_000,
        };
        save(dir.path(), &cache).unwrap();
        assert_eq!(load(dir.path()).unwrap(), cache);
    }

    #[test]
    fn legacy_cache_without_uncovered_loads_as_none() {
        // A cache written before the `uncovered` field existed must still load
        // (serde default), yielding `None` — "unknown", so startup backfills —
        // rather than erroring or masquerading as a computed-empty result.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cache_path(dir.path()),
            r#"{"roots":["/workspace"],"repos":[],"scanned_at":1700000000}"#,
        )
        .unwrap();
        let c = load(dir.path()).expect("legacy cache must load");
        assert!(c.uncovered.is_none());
        assert_eq!(c.scanned_at, 1_700_000_000);
    }

    #[test]
    fn load_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn load_corrupt_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(cache_path(dir.path()), b"{ not json").unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn v1_cache_loads_with_version_zero_and_empty_hits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            cache_path(dir.path()),
            r#"{"roots":["/w"],"repos":[{"path":"/w/a","marker_files":[],"marker_globs":[],
                "package_json_deps":[],"languages":[]}],"uncovered":[],"scanned_at":1}"#,
        )
        .unwrap();
        let c = load(dir.path()).expect("v1 cache must load");
        assert_eq!(c.version, 0);
        assert!(c.repos[0].rule_hits.is_empty());
        assert!(c.repos[0].override_names.is_none());
    }

    #[test]
    fn v2_round_trips_rule_hits() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = repo("/w/a");
        r.rule_hits.insert("glob:*.vue".into(), true);
        r.override_names = Some(vec!["frontend".into()]);
        let cache = ScanCache {
            version: SCAN_CACHE_VERSION,
            roots: vec!["/w".into()],
            repos: vec![r],
            uncovered: Some(vec![]),
            scanned_at: 1,
        };
        save(dir.path(), &cache).unwrap();
        assert_eq!(load(dir.path()).unwrap(), cache);
    }
}
