//! Persist the last repo scan so the Profile view shows meaningful counts on
//! reopen without re-walking the filesystem at startup (Stage B keeps startup
//! walk-free). Derived data — lives in the app-state dir, not in profiles.json.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::profile::discover::RepoSignal;
use crate::util::atomicfile;

/// The cached result of one `s`/Rescan: which roots were walked, what repos were
/// found, and when (epoch seconds). Keyed on `roots` — a cache whose roots no
/// longer match the current scan roots is ignored on load.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanCache {
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
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ScanCache {
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
}
