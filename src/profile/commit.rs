use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use crate::profile::config::Profiles;
use crate::profile::plugins::managed_keys;
use crate::profile::{apply, author, detect};

pub struct CommitReport {
    pub profiles_path: PathBuf,
    #[allow(dead_code)]
    pub global_disabled: usize,
    #[allow(dead_code)]
    pub global_kept: usize,
    pub repos_applied: usize,
}

/// Write profiles.json (+ backup), sync global settings.json (Model C), then
/// apply each selected repo's settings.local.json. One commit, in this order.
pub fn commit(
    cfg_path: &Path,
    settings_path: &Path,
    working: &Profiles,
    repos: &[PathBuf],
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

    let mut repos_applied = 0usize;
    for repo in repos {
        let matched = detect::detect_profiles(repo, working);
        apply::apply(repo, working, &matched)?;
        repos_applied += 1;
    }

    Ok(CommitReport {
        profiles_path: cfg_path.to_path_buf(),
        global_disabled: disabled,
        global_kept: kept,
        repos_applied,
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
        // a rust repo to apply to
        let repo = home.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let rep = commit(
            &cfg_path,
            &settings,
            &working(),
            std::slice::from_ref(&repo),
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
    }

    #[test]
    fn commit_with_zero_repos_still_writes_profiles_and_global() {
        let home = tempfile::tempdir().unwrap();
        let cfg_path = home.path().join("profiles.json");
        let settings = home.path().join("settings.json");
        let rep = commit(&cfg_path, &settings, &working(), &[], 100).unwrap();
        assert!(cfg_path.exists());
        assert!(settings.exists());
        assert_eq!(rep.repos_applied, 0);
    }
}
