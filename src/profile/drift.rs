use std::collections::BTreeSet;
use std::path::Path;

use crate::profile::config::Profiles;
use crate::profile::detect;
use crate::profile::discover::{Inventory, RepoSignal};
use crate::profile::plugins::managed_keys;

/// Managed plugin keys that are no longer installed (dead references in the config).
pub fn stale_refs(inv: &Inventory, working: &Profiles) -> Vec<String> {
    let installed: BTreeSet<&str> = inv.plugins.iter().map(|p| p.key.as_str()).collect();
    let mut out: Vec<String> = managed_keys(working)
        .into_iter()
        .filter(|k| !installed.contains(k.as_str()))
        .collect();
    out.sort();
    out
}

/// Managed, profile-specific (non-universal) plugins that are still enabled at
/// global scope — i.e. global settings drifted from what this config expects.
pub fn global_drift(working: &Profiles, global_enabled: &[String]) -> Vec<String> {
    let universal: BTreeSet<&str> = working.universal.iter().map(String::as_str).collect();
    let enabled: BTreeSet<&str> = global_enabled.iter().map(String::as_str).collect();
    let mut out: Vec<String> = managed_keys(working)
        .into_iter()
        .filter(|k| !universal.contains(k.as_str()) && enabled.contains(k.as_str()))
        .collect();
    out.sort();
    out
}

/// Scanned repos that match no profile in `working` (sorted by path).
pub fn uncovered_repos(inv: &Inventory, working: &Profiles) -> Vec<String> {
    let mut out: Vec<String> = inv
        .repos
        .iter()
        .filter(|r| detect::detect_profiles(Path::new(&r.path), working).is_empty())
        .map(|r| r.path.clone())
        .collect();
    out.sort();
    out
}

/// Uncovered repos computed purely from indexed signals — zero filesystem I/O,
/// via `signal_detect::detect_from_signal`. A repo whose evaluation could not
/// be decided for every profile (some rule needed an atom the index never
/// recorded) is left out of the list rather than guessed at; the second
/// return value is `true` when any repo is still undecided, telling the
/// caller the result is provisional until a full atom index lands.
pub fn uncovered_from_signals(repos: &[RepoSignal], cfg: &Profiles) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut any_pending = false;
    for r in repos {
        let (matched, pending) = crate::profile::signal_detect::detect_from_signal(r, cfg);
        if pending {
            any_pending = true;
            continue;
        }
        if matched.is_empty() {
            out.push(r.path.clone());
        }
    }
    out.sort();
    (out, any_pending)
}

/// The five re-edit drift signals.
pub struct Drift {
    pub new_unassigned: Vec<String>,
    pub stale: Vec<String>,
    pub uncovered: Vec<String>,
    pub global: Vec<String>,
    /// Managed plugins that are installed but lack a `scope: user` entry. This
    /// is the only visible symptom of the failure mode plugin-owned hooks
    /// cannot repair themselves — see `src/doctor.rs`.
    pub scope: Vec<String>,
}

impl Drift {
    pub fn review_count(&self) -> usize {
        self.new_unassigned.len()
            + self.stale.len()
            + self.uncovered.len()
            + self.global.len()
            + self.scope.len()
    }

    // Only called from tests; the binary render path uses review_count() directly.
    #[allow(dead_code)]
    pub fn is_clean(&self) -> bool {
        self.review_count() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profiles;
    use crate::profile::discover::{Inventory, PluginInfo};

    fn inv(keys: &[&str]) -> Inventory {
        Inventory {
            plugins: keys
                .iter()
                .map(|k| PluginInfo {
                    key: k.to_string(),
                    scopes: vec![],
                    description: None,
                })
                .collect(),
            repos: vec![],
            suggested_profiles: vec![],
        }
    }
    fn cfg() -> Profiles {
        serde_json::from_str(
            r#"{"universal":["serena@x"],"profiles":{"rust":{"plugins":["ra@x","gone@x"],"detect":{}}}}"#,
        ).unwrap()
    }

    #[test]
    fn stale_refs_are_managed_minus_installed() {
        // installed: serena, ra (NOT gone). managed: serena, ra, gone. stale = [gone].
        let got = stale_refs(&inv(&["serena@x", "ra@x"]), &cfg());
        assert_eq!(got, vec!["gone@x".to_string()]);
    }

    #[test]
    fn global_drift_is_nonuniversal_managed_enabled_globally() {
        // managed: serena(universal), ra, gone. global currently enables ra@x and serena@x.
        // serena is universal (allowed); ra is profile-specific but globally enabled → drift.
        let got = global_drift(&cfg(), &["serena@x".to_string(), "ra@x".to_string()]);
        assert_eq!(got, vec!["ra@x".to_string()]);
    }

    #[test]
    fn uncovered_repos_are_those_matching_no_profile() {
        use crate::profile::discover::RepoSignal;
        let tmp = tempfile::tempdir().unwrap();
        let rust = tmp.path().join("rusty");
        std::fs::create_dir_all(&rust).unwrap();
        std::fs::write(rust.join("Cargo.toml"), "[package]").unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();

        let mut inv = inv(&[]);
        inv.repos = vec![
            RepoSignal {
                path: rust.display().to_string(),
                marker_files: vec!["Cargo.toml".into()],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            },
            RepoSignal {
                path: plain.display().to_string(),
                marker_files: vec![],
                marker_globs: vec![],
                package_json_deps: vec![],
                languages: vec![],
                rule_hits: Default::default(),
                override_names: None,
            },
        ];
        let working: Profiles = serde_json::from_str(
            r#"{"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let got = uncovered_repos(&inv, &working);
        // the rust repo matches; the plain repo does not.
        assert_eq!(got.len(), 1);
        assert!(got[0].ends_with("plain"));
    }

    #[test]
    fn review_count_sums_all_four() {
        let d = Drift {
            new_unassigned: vec!["a".into()],
            stale: vec!["b".into(), "c".into()],
            uncovered: vec![],
            global: vec!["d".into()],
            scope: vec![],
        };
        assert_eq!(d.review_count(), 4);
        assert!(!d.is_clean());
        assert!(Drift {
            new_unassigned: vec![],
            stale: vec![],
            uncovered: vec![],
            global: vec![],
            scope: vec![],
        }
        .is_clean());
    }

    #[test]
    fn review_count_includes_scope_drift() {
        let d = Drift {
            new_unassigned: vec![],
            stale: vec![],
            uncovered: vec![],
            global: vec![],
            scope: vec!["cc-loadout@cc-loadout".to_string()],
        };
        assert_eq!(d.review_count(), 1);
        assert!(!d.is_clean());
    }

    fn sig(path: &str, hits: &[(&str, bool)]) -> RepoSignal {
        RepoSignal {
            path: path.to_string(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: hits.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            override_names: None,
        }
    }

    fn sig_no_hits(path: &str) -> RepoSignal {
        sig(path, &[])
    }

    #[test]
    fn uncovered_from_signals_excludes_pending_repos() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal": [], "profiles": {
        "rust": {"plugins": [], "detect": {"marker_files": ["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let hit = |b| sig("/a", &[("file:Cargo.toml", b)]);
        let unknown = sig_no_hits("/b"); // empty rule_hits → Unknown
        let (unc, pending) = uncovered_from_signals(&[hit(false), unknown], &cfg);
        assert_eq!(unc, vec!["/a".to_string()]); // definite no-match
        assert!(pending); // /b is undecided, not "uncovered"
    }
}
