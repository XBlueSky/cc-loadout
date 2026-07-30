use std::collections::{BTreeMap, BTreeSet};

use crate::profile::author;
use crate::profile::config::Profiles;
use crate::profile::discover::{Inventory, RepoSignal, SharedSignals};
use crate::profile::plugins::managed_keys;

/// Seed an editable config from a scan: one profile per suggested cluster, its
/// detection from the cluster's shared signals, and empty plugins (no pre-assignment).
/// All installed plugins are left unassigned for the user to organize manually.
#[allow(dead_code)]
pub fn scan_draft(inv: &Inventory, scan_roots: Vec<String>) -> Profiles {
    let mut profiles = BTreeMap::new();
    for sp in &inv.suggested_profiles {
        profiles.insert(
            sp.name.clone(),
            author::profile_from(Vec::new(), &sp.shared_signals),
        );
    }
    author::build_profiles(scan_roots, Vec::new(), profiles)
}

/// Derive the sorted, deduped union of marker_files, marker_globs, and
/// package_json_deps across the given repos.
#[allow(dead_code)]
pub fn signals_from_repos(repos: &[&RepoSignal]) -> SharedSignals {
    let mut mf = BTreeSet::new();
    let mut mg = BTreeSet::new();
    let mut dp = BTreeSet::new();
    for r in repos {
        mf.extend(r.marker_files.iter().cloned());
        mg.extend(r.marker_globs.iter().cloned());
        dp.extend(r.package_json_deps.iter().cloned());
    }
    SharedSignals {
        marker_files: mf.into_iter().collect(),
        marker_globs: mg.into_iter().collect(),
        package_json_deps: dp.into_iter().collect(),
    }
}

/// Return installed plugin keys not in managed_keys(working) and not in
/// on_demand, sorted. (on_demand is a deliberate, acknowledged bucket — it
/// must not re-appear as "needs triage".)
#[allow(dead_code)]
pub fn unassigned_keys(inv: &Inventory, working: &Profiles) -> Vec<String> {
    let managed: BTreeSet<String> = managed_keys(working).into_iter().collect();
    let on_demand: BTreeSet<&str> = working.on_demand.iter().map(String::as_str).collect();
    let mut out: Vec<String> = inv
        .plugins
        .iter()
        .map(|p| p.key.clone())
        .filter(|k| !managed.contains(k) && !on_demand.contains(k.as_str()))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::discover::{Inventory, PluginInfo, SharedSignals, SuggestedProfile};

    fn inv() -> Inventory {
        Inventory {
            plugins: vec![
                PluginInfo {
                    key: "rust-analyzer@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "eslint@x".into(),
                    scopes: vec![],
                    description: None,
                },
                PluginInfo {
                    key: "serena@x".into(),
                    scopes: vec![],
                    description: None,
                },
            ],
            repos: vec![],
            suggested_profiles: vec![
                SuggestedProfile {
                    name: "rust".into(),
                    repos: vec!["/a".into()],
                    shared_signals: SharedSignals {
                        marker_files: vec!["Cargo.toml".into()],
                        ..Default::default()
                    },
                },
                SuggestedProfile {
                    name: "frontend".into(),
                    repos: vec!["/b".into()],
                    shared_signals: SharedSignals {
                        marker_globs: vec!["*.vue".into()],
                        ..Default::default()
                    },
                },
            ],
        }
    }

    #[test]
    fn scan_draft_creates_empty_profiles_no_preassignment() {
        let cfg = scan_draft(&inv(), vec!["/root".into()]);
        assert_eq!(cfg.scan_roots, vec!["/root".to_string()]);
        assert!(cfg.universal.is_empty());
        // suggested profiles exist as EMPTY buckets, carrying detect:
        assert!(cfg.profiles["rust"].plugins.is_empty());
        assert_eq!(
            cfg.profiles["rust"].detect.marker_files,
            vec!["Cargo.toml".to_string()]
        );
        assert!(cfg.profiles["frontend"].plugins.is_empty());
        // every installed plugin is unassigned now:
        let unassigned = unassigned_keys(&inv(), &cfg);
        assert_eq!(
            unassigned,
            vec![
                "eslint@x".to_string(),
                "rust-analyzer@x".to_string(),
                "serena@x".to_string()
            ]
        );
    }

    #[test]
    fn signals_from_repos_unions_and_sorts() {
        use crate::profile::discover::RepoSignal;
        let a = RepoSignal {
            path: "/a".into(),
            marker_files: vec!["Cargo.toml".into()],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        };
        let b = RepoSignal {
            path: "/b".into(),
            marker_files: vec!["Cargo.toml".into(), "build.rs".into()],
            marker_globs: vec!["*.rs".into()],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: Default::default(),
            override_names: None,
        };
        let s = signals_from_repos(&[&a, &b]);
        assert_eq!(
            s.marker_files,
            vec!["Cargo.toml".to_string(), "build.rs".to_string()]
        ); // sorted+dedup
        assert_eq!(s.marker_globs, vec!["*.rs".to_string()]);
    }

    #[test]
    fn unassigned_is_installed_minus_managed() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal":["serena@x"],"profiles":{"rust":{"plugins":["rust-analyzer@x"],"detect":{}}}}"#,
        )
        .unwrap();
        let got = unassigned_keys(&inv(), &cfg);
        assert_eq!(got, vec!["eslint@x".to_string()]); // rust-analyzer (profile) + serena (universal) assigned
    }

    #[test]
    fn unassigned_excludes_on_demand() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal":["serena@x"],"profiles":{},"on_demand":["eslint@x"]}"#,
        )
        .unwrap();
        // inv() (already defined above in this test module) has: rust-analyzer@x,
        // eslint@x, serena@x. serena is universal, eslint is on_demand -> only
        // rust-analyzer@x should remain unassigned.
        let got = unassigned_keys(&inv(), &cfg);
        assert_eq!(got, vec!["rust-analyzer@x".to_string()]);
    }
}
