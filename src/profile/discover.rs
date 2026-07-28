use crate::profile::{detect, scan};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PluginInfo {
    pub key: String,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoSignal {
    pub path: String,
    pub marker_files: Vec<String>,
    pub marker_globs: Vec<String>,
    pub package_json_deps: Vec<String>,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SharedSignals {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub marker_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub marker_globs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub package_json_deps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SuggestedProfile {
    pub name: String,
    pub repos: Vec<String>,
    pub shared_signals: SharedSignals,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub plugins: Vec<PluginInfo>,
    pub repos: Vec<RepoSignal>,
    pub suggested_profiles: Vec<SuggestedProfile>,
}

/// Map `<name>@<marketplace>` -> description, read from each
/// `<plugins>/marketplaces/<marketplace>/.claude-plugin/marketplace.json`.
/// Missing/malformed files are skipped (never errors).
pub fn plugin_descriptions(registry_path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(plugins_dir) = registry_path.parent() else {
        return out;
    };
    let mp_root = plugins_dir.join("marketplaces");
    let entries = match std::fs::read_dir(&mp_root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for ent in entries.flatten() {
        let mp_name = ent.file_name().to_string_lossy().into_owned();
        let manifest = ent.path().join(".claude-plugin").join("marketplace.json");
        let text = match std::fs::read_to_string(&manifest) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(arr) = json.get("plugins").and_then(|p| p.as_array()) {
            for p in arr {
                if let (Some(name), Some(desc)) = (
                    p.get("name").and_then(|n| n.as_str()),
                    p.get("description").and_then(|d| d.as_str()),
                ) {
                    out.insert(format!("{name}@{mp_name}"), desc.to_string());
                }
            }
        }
    }
    out
}

/// Parse `installed_plugins.json` into a sorted list of `{ key, scopes, description }`.
/// Missing or malformed file → empty vec (never panics).
pub fn list_plugins(registry_path: &Path) -> Vec<PluginInfo> {
    let text = match std::fs::read_to_string(registry_path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let map = match json.get("plugins").and_then(|p| p.as_object()) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let descs = plugin_descriptions(registry_path);
    let mut out: Vec<PluginInfo> = map
        .iter()
        .map(|(key, entries)| {
            let mut scopes: Vec<String> = entries
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.get("scope").and_then(|s| s.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            scopes.sort();
            scopes.dedup();
            PluginInfo {
                key: key.clone(),
                scopes,
                description: descs.get(key).cloned(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

const MARKER_FILES: &[&str] = &[
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "pom.xml",
    "Gemfile",
    "composer.json",
    "build.gradle",
];
const KNOWN_GLOBS: &[&str] = &["*.vue"];

/// Scan every git repo under `roots` and extract raw, detect-schema-aligned signals.
pub fn scan_repo_signals(roots: &[String], max_depth: usize) -> Vec<RepoSignal> {
    let mut out = Vec::new();
    for root in roots {
        let root = Path::new(root);
        if !root.is_dir() {
            continue;
        }
        for repo in scan::find_git_repos(root, max_depth) {
            out.push(signals_for_repo(&repo));
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn signals_for_repo(repo: &Path) -> RepoSignal {
    let marker_files = MARKER_FILES
        .iter()
        .filter(|m| repo.join(m).exists())
        .map(|m| m.to_string())
        .collect();
    let marker_globs = KNOWN_GLOBS
        .iter()
        .filter(|g| detect::glob_exists(repo, g))
        .map(|g| g.to_string())
        .collect();
    let package_json_deps = if repo.join("package.json").exists() {
        read_package_json_deps(repo)
    } else {
        Vec::new()
    };
    RepoSignal {
        path: repo.display().to_string(),
        marker_files,
        marker_globs,
        package_json_deps,
        languages: root_extensions(repo),
    }
}

fn read_package_json_deps(repo: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(repo.join("package.json")) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut deps = BTreeSet::new();
    for field in ["dependencies", "devDependencies"] {
        if let Some(obj) = json.get(field).and_then(|d| d.as_object()) {
            deps.extend(obj.keys().cloned());
        }
    }
    deps.into_iter().collect()
}

fn root_extensions(repo: &Path) -> Vec<String> {
    let mut exts = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(repo) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                if let Some(ext) = e.path().extension().and_then(|x| x.to_str()) {
                    exts.insert(ext.to_string());
                }
            }
        }
    }
    exts.into_iter().collect()
}

const FRONTEND_DEPS: &[&str] = &["react", "vue", "svelte", "preact", "solid-js"];

/// Cluster repos by a coarse signal key and propose one profile per cluster.
/// Pure and fully overridable by the caller (TUI / agent).
pub fn suggest_profiles(repos: &[RepoSignal]) -> Vec<SuggestedProfile> {
    let mut groups: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for r in repos {
        if let Some(key) = cluster_key(r) {
            groups.entry(key).or_default().push(r.path.clone());
        }
    }
    groups
        .into_iter()
        .map(|(key, mut paths)| {
            paths.sort();
            SuggestedProfile {
                name: key.to_string(),
                repos: paths,
                shared_signals: defining_signals(key),
            }
        })
        .collect()
}

fn cluster_key(r: &RepoSignal) -> Option<&'static str> {
    let has_marker = |m: &str| r.marker_files.iter().any(|f| f == m);
    if r.marker_globs.iter().any(|g| g == "*.vue")
        || r.package_json_deps
            .iter()
            .any(|d| FRONTEND_DEPS.contains(&d.as_str()))
    {
        return Some("frontend");
    }
    if has_marker("Cargo.toml") {
        return Some("rust");
    }
    if has_marker("pyproject.toml") || has_marker("requirements.txt") {
        return Some("python");
    }
    if has_marker("go.mod") {
        return Some("go");
    }
    if has_marker("package.json") {
        return Some("node");
    }
    // pom.xml, Gemfile, composer.json, and build.gradle are still collected into
    // marker_files for the raw inventory, but are intentionally NOT clustered in
    // this milestone — java/ruby/php cluster keys are deferred to a later milestone.
    None
}

/// Return the cluster's COMPLETE detection rule — the union of the signal
/// family for that cluster, which `detect` OR-matches. This is intentionally
/// broader than any single repo (e.g. the `frontend` rule lists both `*.vue`
/// and the full `FRONTEND_DEPS` family) so the adopted profile also matches
/// repos the user adds later. It is a per-CLUSTER rule, not a per-repo claim.
fn defining_signals(key: &str) -> SharedSignals {
    let mut s = SharedSignals::default();
    match key {
        "frontend" => {
            s.marker_globs = vec!["*.vue".into()];
            s.package_json_deps = FRONTEND_DEPS.iter().map(|d| d.to_string()).collect();
        }
        "rust" => s.marker_files = vec!["Cargo.toml".into()],
        "python" => s.marker_files = vec!["pyproject.toml".into(), "requirements.txt".into()],
        "go" => s.marker_files = vec!["go.mod".into()],
        "node" => s.marker_files = vec!["package.json".into()],
        _ => {}
    }
    s
}

/// `<config_dir>/plugins/installed_plugins.json`, where config_dir is
/// `$CLAUDE_CONFIG_DIR` (if set) else `~/.claude`. Mirrors account::paths.
pub fn resolve_registry_path(home: &Path, config_override: Option<&Path>) -> PathBuf {
    let base = match config_override {
        Some(p) => p.to_path_buf(),
        None => home.join(".claude"),
    };
    base.join("plugins").join("installed_plugins.json")
}

/// Assemble the full inventory: plugins ∪ repo signals ∪ suggested profiles.
pub fn build_inventory(registry_path: &Path, roots: &[String], max_depth: usize) -> Inventory {
    let plugins = list_plugins(registry_path);
    let repos = scan_repo_signals(roots, max_depth);
    let suggested_profiles = suggest_profiles(&repos);
    Inventory {
        plugins,
        repos,
        suggested_profiles,
    }
}

/// Assemble a plugins-only inventory: read the registry (+ marketplace
/// descriptions) but DO NOT walk the filesystem. `repos` and
/// `suggested_profiles` start empty. The TUI uses this at startup so the board
/// opens without an eager scan; repos are filled later by the explicit `s`
/// scan action. The CLI keeps using `build_inventory` (which scans).
pub fn build_inventory_no_scan(registry_path: &Path) -> Inventory {
    Inventory {
        plugins: list_plugins(registry_path),
        repos: Vec::new(),
        suggested_profiles: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_registry_path_default_and_override() {
        assert_eq!(
            resolve_registry_path(Path::new("/home/u"), None),
            Path::new("/home/u/.claude/plugins/installed_plugins.json")
        );
        assert_eq!(
            resolve_registry_path(Path::new("/home/u"), Some(Path::new("/cfg"))),
            Path::new("/cfg/plugins/installed_plugins.json")
        );
    }

    #[test]
    fn build_inventory_combines_all_three() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("installed_plugins.json");
        std::fs::write(
            &reg,
            r#"{"plugins":{"serena@official":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        let roots = tempfile::tempdir().unwrap();
        let repo = roots.path().join("app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

        let inv = build_inventory(&reg, &[roots.path().display().to_string()], 6);
        assert_eq!(inv.plugins.len(), 1);
        assert_eq!(inv.repos.len(), 1);
        assert_eq!(inv.suggested_profiles.len(), 1);
        assert_eq!(inv.suggested_profiles[0].name, "rust");
    }

    fn sig(path: &str, deps: &[&str], globs: &[&str], markers: &[&str]) -> RepoSignal {
        RepoSignal {
            path: path.into(),
            marker_files: markers.iter().map(|s| s.to_string()).collect(),
            marker_globs: globs.iter().map(|s| s.to_string()).collect(),
            package_json_deps: deps.iter().map(|s| s.to_string()).collect(),
            languages: vec![],
        }
    }

    #[test]
    fn suggest_profiles_clusters_by_signal() {
        let repos = vec![
            sig("/a", &["react"], &[], &["package.json"]),
            sig("/b", &[], &["*.vue"], &["package.json"]),
            sig("/c", &[], &[], &["Cargo.toml"]),
        ];
        let got = suggest_profiles(&repos);
        assert_eq!(got.len(), 2);
        // Output is ordered by cluster name (BTreeMap), so `frontend` sorts before `rust`.
        assert_eq!(got[0].name, "frontend");
        assert_eq!(got[0].repos, vec!["/a".to_string(), "/b".to_string()]);
        assert_eq!(
            got[0].shared_signals.package_json_deps,
            vec![
                "react".to_string(),
                "vue".to_string(),
                "svelte".to_string(),
                "preact".to_string(),
                "solid-js".to_string()
            ]
        );
        assert_eq!(got[1].name, "rust");
        assert_eq!(
            got[1].shared_signals.marker_files,
            vec!["Cargo.toml".to_string()]
        );
    }

    #[test]
    fn scan_repo_signals_extracts_markers_and_deps() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("app");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(
            repo.join("package.json"),
            r#"{"dependencies":{"react":"18"},"devDependencies":{"vite":"5"}}"#,
        )
        .unwrap();
        std::fs::write(repo.join("src.vue"), "x").unwrap();

        let got = scan_repo_signals(&[root.path().display().to_string()], 6);
        assert_eq!(got.len(), 1);
        let s = &got[0];
        assert!(s.path.ends_with("app"));
        assert_eq!(s.marker_files, vec!["package.json".to_string()]);
        assert_eq!(s.marker_globs, vec!["*.vue".to_string()]);
        assert_eq!(
            s.package_json_deps,
            vec!["react".to_string(), "vite".to_string()]
        );
        assert!(s.languages.contains(&"json".to_string()));
    }

    #[test]
    fn list_plugins_reads_keys_and_dedups_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("installed_plugins.json");
        std::fs::write(
            &p,
            r#"{
            "plugins": {
                "serena@official": [{"scope":"user"},{"scope":"user"}],
                "rag@ai": [{"scope":"local"}]
            }
        }"#,
        )
        .unwrap();
        let got = list_plugins(&p);
        assert_eq!(
            got,
            vec![
                PluginInfo {
                    key: "rag@ai".into(),
                    scopes: vec!["local".into()],
                    description: None,
                },
                PluginInfo {
                    key: "serena@official".into(),
                    scopes: vec!["user".into()],
                    description: None,
                },
            ]
        );
    }

    #[test]
    fn list_plugins_missing_file_is_empty() {
        assert!(list_plugins(Path::new("/no/such/file.json")).is_empty());
    }

    #[test]
    fn plugin_descriptions_reads_marketplace_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let reg = plugins.join("installed_plugins.json");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(
            &reg,
            r#"{"plugins":{"serena@official":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        let mp = plugins
            .join("marketplaces")
            .join("official")
            .join(".claude-plugin");
        std::fs::create_dir_all(&mp).unwrap();
        std::fs::write(mp.join("marketplace.json"),
            r#"{"name":"official","plugins":[{"name":"serena","description":"Semantic code nav"},{"name":"other","description":"x"}]}"#).unwrap();

        let descs = plugin_descriptions(&reg);
        assert_eq!(
            descs.get("serena@official").map(String::as_str),
            Some("Semantic code nav")
        );

        let plugs = list_plugins(&reg);
        assert_eq!(plugs[0].key, "serena@official");
        assert_eq!(plugs[0].description.as_deref(), Some("Semantic code nav"));
    }

    #[test]
    fn plugin_descriptions_missing_manifest_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("installed_plugins.json");
        std::fs::write(&reg, r#"{"plugins":{"x@nope":[{"scope":"user"}]}}"#).unwrap();
        assert!(plugin_descriptions(&reg).is_empty());
        assert_eq!(list_plugins(&reg)[0].description, None);
    }

    #[test]
    fn build_inventory_no_scan_reads_plugins_but_skips_repos() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("installed_plugins.json");
        std::fs::write(
            &reg,
            r#"{"plugins":{"serena@official":[{"scope":"user"}]}}"#,
        )
        .unwrap();
        let inv = build_inventory_no_scan(&reg);
        assert_eq!(inv.plugins.len(), 1, "plugins must still be read");
        assert_eq!(inv.plugins[0].key, "serena@official");
        assert!(
            inv.repos.is_empty(),
            "no-scan inventory must not walk repos"
        );
        assert!(
            inv.suggested_profiles.is_empty(),
            "no-scan inventory must not suggest profiles"
        );
    }
}
