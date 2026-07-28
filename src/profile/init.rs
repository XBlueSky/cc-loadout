use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::profile::config::Profile;
use crate::profile::plugins::managed_keys;
use crate::profile::{apply, author, discover};

/// The AI/script's decision: which plugins are universal, and which plugins each
/// (scan-suggested) profile gets. Profile keys are suggested-profile names.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    #[serde(default)]
    pub universal: Vec<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Vec<String>>,
}

/// Strict validation against the scanned inventory: every profile name must be a
/// suggested profile, every plugin key must be installed. Collects all problems.
pub(crate) fn validate(a: &Assignment, inv: &discover::Inventory) -> Result<()> {
    if a.universal.is_empty() && a.profiles.is_empty() {
        bail!("assignment is empty: set `universal` and/or `profiles`");
    }

    let suggested: Vec<&str> = inv
        .suggested_profiles
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    let installed: BTreeSet<&str> = inv.plugins.iter().map(|p| p.key.as_str()).collect();

    let mut errs: Vec<String> = Vec::new();
    for name in a.profiles.keys() {
        if !suggested.contains(&name.as_str()) {
            errs.push(format!(
                "unknown profile '{name}' (suggested names: {})",
                if suggested.is_empty() {
                    "<none>".to_string()
                } else {
                    suggested.join(", ")
                }
            ));
        }
    }
    let mut keys: Vec<&String> = a.universal.iter().collect();
    for plugins in a.profiles.values() {
        keys.extend(plugins.iter());
    }
    for key in keys {
        if !installed.contains(key.as_str()) {
            errs.push(format!("unknown plugin '{key}' (not installed)"));
        }
    }
    if !errs.is_empty() {
        bail!("invalid assignment:\n  {}", errs.join("\n  "));
    }
    Ok(())
}

/// Assemble a Profiles from a validated assignment: each profile's plugins come
/// from the assignment, its detect from the matching scanned cluster's signals.
pub(crate) fn assemble_profiles(
    a: &Assignment,
    inv: &discover::Inventory,
    scan_roots: Vec<String>,
) -> crate::profile::config::Profiles {
    let mut map: BTreeMap<String, Profile> = BTreeMap::new();
    for (name, plugins) in &a.profiles {
        if let Some(suggested) = inv.suggested_profiles.iter().find(|s| &s.name == name) {
            map.insert(
                name.clone(),
                author::profile_from(plugins.clone(), &suggested.shared_signals),
            );
        }
    }
    author::build_profiles(scan_roots, a.universal.clone(), map)
}

/// Headless `profile init`: scan `root`, validate the assignment, write
/// profiles.json (auto-backed-up) and apply the global plugin state. Reports a
/// human summary, or a machine-readable object when `json_out`.
pub fn init_noninteractive(
    registry_path: &Path,
    root: &str,
    assign_json: &str,
    cfg_path: &Path,
    settings_path: &Path,
    now: i64,
    json_out: bool,
) -> Result<()> {
    let assign: Assignment = serde_json::from_str(assign_json).context("parsing --assign JSON")?;

    let roots = vec![root.to_string()];
    let inv = discover::build_inventory(registry_path, &roots, 6);
    validate(&assign, &inv)?;

    let mut cfg = assemble_profiles(&assign, &inv, vec![root.to_string()]);

    // `--assign` has no vocabulary for `on_demand` (it's a config-editing
    // concern, not something a scan-assignment expresses) — carry over
    // whatever was already there rather than silently dropping it when
    // `assemble_profiles` builds a fresh `Profiles` from scratch. If
    // `cfg_path` doesn't exist yet, `cfg.on_demand` stays empty (`Default`).
    if cfg_path.exists() {
        let existing = crate::profile::config::load(cfg_path)
            .with_context(|| format!("loading existing {}", cfg_path.display()))?;
        cfg.on_demand = existing.on_demand;
    }

    author::write_profiles(cfg_path, &cfg, now)?;
    let (before, after) = apply::apply_global(settings_path, &cfg)?;

    if json_out {
        let out = serde_json::json!({
            "profiles_path": cfg_path.display().to_string(),
            "profiles": &cfg,
            "global": { "before": before, "after": after },
            "next_step": "run cc-loadout profile apply --all to activate per-repo",
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        let kept = cfg.universal.len();
        let disabled = managed_keys(&cfg)
            .into_iter()
            .filter(|k| !cfg.universal.contains(k))
            .count();
        println!("wrote {}", cfg_path.display());
        if !cfg.universal.is_empty() {
            println!("  universal: {}", cfg.universal.join(", "));
        }
        for (name, p) in &cfg.profiles {
            println!("  {name}  ({} plugin(s))", p.plugins.len());
        }
        println!("global: disabled {disabled} plugin(s), kept {kept} universal");
        println!("next: run `cc-loadout profile apply --all` to activate per-repo");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a temp home with a registry (the named plugins) and a scan root
    /// containing one git repo with `Cargo.toml` (clusters to a "rust" profile).
    /// Returns (tempdir-guard, registry, scan-root, profiles.json path, settings.json path).
    fn fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let reg = home.join("installed_plugins.json");
        std::fs::write(
            &reg,
            r#"{"plugins":{"serena@official":[{"scope":"user"}],"rust-analyzer@community":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        let root = home.join("repos");
        let repo = root.join("app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
        let cfg = home.join("profiles.json");
        let settings = home.join("settings.json");
        (dir, reg, root, cfg, settings)
    }

    #[test]
    fn writes_profiles_and_applies_global() {
        let (_d, reg, root, cfg, settings) = fixture();
        let assign =
            r#"{"universal":["serena@official"],"profiles":{"rust":["rust-analyzer@community"]}}"#;
        init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap();

        let written: crate::profile::config::Profiles =
            serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(written.universal, vec!["serena@official".to_string()]);
        assert_eq!(
            written.profiles["rust"].plugins,
            vec!["rust-analyzer@community".to_string()]
        );
        assert_eq!(
            written.profiles["rust"].detect.marker_files,
            vec!["Cargo.toml".to_string()]
        );

        let s: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        assert_eq!(
            s["enabledPlugins"]["serena@official"],
            serde_json::json!(true)
        );
        assert_eq!(
            s["enabledPlugins"]["rust-analyzer@community"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn fresh_init_leaves_on_demand_empty() {
        let (_d, reg, root, cfg, settings) = fixture();
        let assign =
            r#"{"universal":["serena@official"],"profiles":{"rust":["rust-analyzer@community"]}}"#;
        init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap();

        let written: crate::profile::config::Profiles =
            serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        assert!(
            written.on_demand.is_empty(),
            "fresh init with no prior file must leave on_demand empty"
        );
    }

    #[test]
    fn rerunning_init_preserves_existing_on_demand() {
        let (_d, reg, root, cfg, settings) = fixture();
        // Seed an existing profiles.json as if the user had already run
        // `profile edit -> Assign -> On-demand` before rerunning `init --assign`.
        std::fs::write(
            &cfg,
            r#"{"scan_roots":[],"universal":[],"profiles":{},"on_demand":["pixijs-skills@x"]}"#,
        )
        .unwrap();

        let assign =
            r#"{"universal":["serena@official"],"profiles":{"rust":["rust-analyzer@community"]}}"#;
        init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap();

        let written: crate::profile::config::Profiles =
            serde_json::from_slice(&std::fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(
            written.on_demand,
            vec!["pixijs-skills@x".to_string()],
            "--assign rerun must carry over the prior on_demand list untouched"
        );
        // Sanity: the new assignment still took effect otherwise.
        assert_eq!(written.universal, vec!["serena@official".to_string()]);
    }

    #[test]
    fn unknown_profile_name_errors_and_writes_nothing() {
        let (_d, reg, root, cfg, settings) = fixture();
        let assign = r#"{"universal":[],"profiles":{"web":["serena@official"]}}"#;
        let err = init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("web"),
            "error names the unknown profile: {err}"
        );
        assert!(
            !cfg.exists(),
            "no profiles.json written on validation failure"
        );
    }

    #[test]
    fn unknown_plugin_key_errors_and_writes_nothing() {
        let (_d, reg, root, cfg, settings) = fixture();
        let assign = r#"{"universal":["ghost@x"],"profiles":{}}"#;
        let err = init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("ghost@x"),
            "error names the unknown plugin: {err}"
        );
        assert!(
            !cfg.exists(),
            "no profiles.json written on validation failure"
        );
    }

    #[test]
    fn unknown_top_level_key_errors_and_writes_nothing() {
        let (_d, reg, root, cfg, settings) = fixture();
        let assign = r#"{"universe":["serena@official"]}"#; // typo: should be "universal"
        let err_chain = init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            assign,
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap_err();
        let full_err = format!("{:?}", err_chain);
        assert!(
            full_err.contains("universe") || full_err.contains("unknown field"),
            "error mentions the bad key (universe): {full_err}"
        );
        assert!(!cfg.exists());
    }

    #[test]
    fn empty_assignment_errors_and_writes_nothing() {
        let (_d, reg, root, cfg, settings) = fixture();
        let err = init_noninteractive(
            &reg,
            root.to_str().unwrap(),
            "{}",
            &cfg,
            &settings,
            100,
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty"), "error explains emptiness: {err}");
        assert!(!cfg.exists());
    }
}
