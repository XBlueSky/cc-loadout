use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContentRule {
    pub file: String,
    pub word: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Detect {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marker_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marker_globs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_json_deps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps_keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentRule>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Profile {
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub detect: Detect,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Profiles {
    #[serde(default)]
    pub scan_roots: Vec<String>,
    #[serde(default)]
    pub universal: Vec<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Plugins available for session-scoped `acquire`/`release` (see
    /// `src/profile/on_demand.rs`). Deliberately NOT fed into
    /// `managed_keys()`/`desired_plugins()` — `apply` must never touch these.
    /// The single exception is `doctor`, which demotes a key left `true` in the
    /// GLOBAL settings.json (see `apply::demotable_on_demand_keys`): that value
    /// is inherited by every repo precisely BECAUSE `apply` leaves the key
    /// absent, so excluding the pool from `apply` is what makes the global
    /// value decisive.
    #[serde(default)]
    pub on_demand: Vec<String>,
}

/// Resolve the profiles path given an optional explicit override (pure/testable).
pub fn resolve_profiles_path(home: &Path, env_override: Option<&Path>) -> PathBuf {
    match env_override {
        Some(p) => p.to_path_buf(),
        None => home.join(".claude").join("profiles").join("profiles.json"),
    }
}

/// `$CC_LOADOUT_PROFILES` or `~/.claude/profiles/profiles.json`.
pub fn profiles_path(home: &Path) -> PathBuf {
    let var = std::env::var_os("CC_LOADOUT_PROFILES").filter(|s| !s.is_empty());
    resolve_profiles_path(home, var.as_deref().map(Path::new))
}

pub fn load(path: &Path) -> Result<Profiles> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profiles.json");
        std::fs::write(&p, br#"{
            "scan_roots": ["/a", "/b"],
            "universal": ["u1@m", "u2@m"],
            "profiles": {
                "backend": {"plugins": ["s1@m"], "detect": {"path_prefixes": ["/a/"], "marker_files": ["INFO"]}},
                "ai-side": {"plugins": ["r1@m"], "detect": {"marker_files": ["requirements.txt"], "deps_keywords": ["openai"]}}
            }
        }"#).unwrap();
        let cfg = load(&p).unwrap();
        assert_eq!(cfg.scan_roots, vec!["/a", "/b"]);
        assert_eq!(cfg.universal.len(), 2);
        assert_eq!(cfg.profiles["backend"].plugins, vec!["s1@m"]);
        assert_eq!(cfg.profiles["backend"].detect.path_prefixes, vec!["/a/"]);
        assert_eq!(cfg.profiles["ai-side"].detect.deps_keywords, vec!["openai"]);
        assert!(cfg.profiles["ai-side"].detect.marker_globs.is_empty());
    }

    #[test]
    fn serializes_and_omits_empty_detect_fields() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"scan_roots":["/x"],"universal":["u@m"],
                "profiles":{"rust":{"plugins":["a@m"],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        let out = serde_json::to_string(&cfg).unwrap();
        assert!(out.contains("\"marker_files\":[\"Cargo.toml\"]"));
        assert!(!out.contains("path_prefixes"));
        assert!(!out.contains("deps_keywords"));
    }

    #[test]
    fn resolve_profiles_path_default_and_override() {
        let home = std::path::Path::new("/home/u");
        assert_eq!(
            resolve_profiles_path(home, None),
            std::path::Path::new("/home/u/.claude/profiles/profiles.json")
        );
        assert_eq!(
            resolve_profiles_path(home, Some(std::path::Path::new("/tmp/x.json"))),
            std::path::Path::new("/tmp/x.json")
        );
    }

    #[test]
    fn parses_and_roundtrips_content_rules() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"ml":{"plugins":[],"detect":{"content":[{"file":"requirements.txt","word":"torch"}]}}}}"#,
        )
        .unwrap();
        assert_eq!(
            cfg.profiles["ml"].detect.content,
            vec![ContentRule {
                file: "requirements.txt".into(),
                word: "torch".into()
            }]
        );
        let out = serde_json::to_string(&cfg).unwrap();
        assert!(
            out.contains(r#""content":[{"file":"requirements.txt","word":"torch"}]"#),
            "content must round-trip; got {out}"
        );
    }

    #[test]
    fn detect_without_content_is_empty_not_error() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"rust":{"plugins":[],"detect":{"marker_files":["Cargo.toml"]}}}}"#,
        )
        .unwrap();
        assert!(cfg.profiles["rust"].detect.content.is_empty());
    }

    #[test]
    fn parses_and_defaults_on_demand() {
        // present in JSON
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal":[],"profiles":{},"on_demand":["pixijs-skills@x"]}"#,
        )
        .unwrap();
        assert_eq!(cfg.on_demand, vec!["pixijs-skills@x".to_string()]);

        // absent in JSON -> defaults to empty, doesn't error
        let cfg2: Profiles = serde_json::from_str(r#"{"universal":[]}"#).unwrap();
        assert!(cfg2.on_demand.is_empty());
    }
}
