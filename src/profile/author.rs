use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::profile::config::{ContentRule, Detect, Profile, Profiles};
use crate::profile::discover::SharedSignals;
use crate::util::atomicfile;

/// Build a `Profile` from chosen plugins and a cluster's detection signals.
/// package.json deps become precise `content` pairs (file = "package.json");
/// legacy `package_json_deps` is no longer produced.
pub fn profile_from(plugins: Vec<String>, signals: &SharedSignals) -> Profile {
    let content = signals
        .package_json_deps
        .iter()
        .map(|dep| ContentRule {
            file: "package.json".to_string(),
            word: dep.clone(),
        })
        .collect();
    Profile {
        plugins,
        detect: Detect {
            marker_files: signals.marker_files.clone(),
            marker_globs: signals.marker_globs.clone(),
            content,
            ..Default::default()
        },
    }
}

/// Assemble the top-level `Profiles` config.
pub fn build_profiles(
    scan_roots: Vec<String>,
    universal: Vec<String>,
    profiles: BTreeMap<String, Profile>,
) -> Profiles {
    Profiles {
        scan_roots,
        universal,
        profiles,
        ..Default::default()
    }
}

/// Write `profiles` to `path` as pretty JSON, atomically. If `path` already
/// exists, copy it to `<path>.bak.<now_epoch>` first.
pub fn write_profiles(path: &Path, profiles: &Profiles, now_epoch: i64) -> Result<()> {
    if path.exists() {
        let base = {
            let mut b = path.as_os_str().to_owned();
            b.push(format!(".bak.{now_epoch}"));
            b
        };
        let mut backup = PathBuf::from(&base);
        let mut n = 1;
        while backup.exists() {
            let mut b = base.clone();
            b.push(format!(".{n}"));
            backup = PathBuf::from(b);
            n += 1;
        }
        std::fs::copy(path, &backup)
            .with_context(|| format!("backing up to {}", backup.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(profiles)?;
    atomicfile::write_atomic(path, &bytes, 0o644)?;
    Ok(())
}

/// Write `profiles` to `path` as pretty JSON, atomically, WITHOUT a timestamped
/// backup. Used by the Profile view's autosave, which fires on every edit — a
/// per-edit `.bak` would spam the directory. The backup-making `write_profiles`
/// stays on the explicit apply/deploy path.
pub fn write_profiles_quiet(path: &Path, profiles: &Profiles) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(profiles)?;
    atomicfile::write_atomic(path, &bytes, 0o644)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_profiles_quiet_writes_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("profiles.json"); // parent must be created
        let cfg = Profiles {
            universal: vec!["u@m".into()],
            ..Default::default()
        };
        write_profiles_quiet(&path, &cfg).unwrap();
        let back = crate::profile::config::load(&path).unwrap();
        assert_eq!(back.universal, vec!["u@m".to_string()]);

        // A second write must NOT create any .bak sibling (autosave fires often).
        write_profiles_quiet(&path, &cfg).unwrap();
        let baks = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .count();
        assert_eq!(baks, 0, "quiet writer must not create .bak files");
    }

    #[test]
    fn profile_from_maps_signals_into_detect() {
        let signals = SharedSignals {
            marker_files: vec!["Cargo.toml".into()],
            marker_globs: vec![],
            package_json_deps: vec![],
        };
        let p = profile_from(vec!["a@m".into()], &signals);
        assert_eq!(p.plugins, vec!["a@m".to_string()]);
        assert_eq!(p.detect.marker_files, vec!["Cargo.toml".to_string()]);
        assert!(p.detect.path_prefixes.is_empty());
    }

    #[test]
    fn profile_from_converts_pkg_deps_to_content_pairs() {
        let signals = SharedSignals {
            package_json_deps: vec!["svelte".into(), "react".into()],
            ..Default::default()
        };
        let p = profile_from(vec![], &signals);
        assert_eq!(
            p.detect.content,
            vec![
                ContentRule {
                    file: "package.json".into(),
                    word: "svelte".into()
                },
                ContentRule {
                    file: "package.json".into(),
                    word: "react".into()
                },
            ]
        );
        assert!(
            p.detect.package_json_deps.is_empty(),
            "new profiles use content, not legacy package_json_deps"
        );
    }

    #[test]
    fn write_profiles_writes_pretty_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");

        let mut profiles = BTreeMap::new();
        profiles.insert(
            "rust".to_string(),
            profile_from(
                vec!["a@m".into()],
                &SharedSignals {
                    marker_files: vec!["Cargo.toml".into()],
                    marker_globs: vec![],
                    package_json_deps: vec![],
                },
            ),
        );
        let cfg = build_profiles(vec!["/x".into()], vec!["u@m".into()], profiles);

        write_profiles(&path, &cfg, 111).unwrap();
        let reloaded = crate::profile::config::load(&path).unwrap();
        assert_eq!(reloaded.universal, vec!["u@m".to_string()]);
        assert_eq!(
            reloaded.profiles["rust"].detect.marker_files,
            vec!["Cargo.toml".to_string()]
        );

        write_profiles(&path, &cfg, 222).unwrap();
        let backup = dir.path().join("profiles.json.bak.222");
        assert!(backup.exists(), "expected backup at {}", backup.display());

        // third write at the same epoch must not clobber the existing backup
        write_profiles(&path, &cfg, 222).unwrap();
        assert!(dir.path().join("profiles.json.bak.222.1").exists());
    }
}
