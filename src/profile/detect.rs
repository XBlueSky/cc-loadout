use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

use crate::profile::config::{Profile, Profiles};

/// Why a profile matched: the first short-circuit rule + the specific value that
/// fired (`value` is None for an override-file match).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchReason {
    pub rule: &'static str,
    pub value: Option<String>,
}

/// Matched profile names (deduped, sorted). Thin name-only view over
/// `detect_profiles_explained` — kept so `apply`/`status` callers are unaffected.
pub fn detect_profiles(root: &Path, cfg: &Profiles) -> Vec<String> {
    detect_profiles_explained(root, cfg)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Matched profiles WITH provenance (first matching rule + value per profile),
/// sorted by profile name. 1:1 with `detect_profiles` on names.
pub fn detect_profiles_explained(root: &Path, cfg: &Profiles) -> Vec<(String, MatchReason)> {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root = canonical.as_path();
    let override_file = root.join(".claude").join("profile");
    if override_file.is_file() {
        if let Ok(text) = std::fs::read_to_string(&override_file) {
            let mut names: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect();
            names.sort();
            names.dedup();
            return names
                .into_iter()
                .map(|n| {
                    (
                        n,
                        MatchReason {
                            rule: "override",
                            value: None,
                        },
                    )
                })
                .collect();
        }
    }

    let mut matched: Vec<(String, MatchReason)> = Vec::new();
    for (name, profile) in &cfg.profiles {
        if let Some(reason) = detect_one(root, profile) {
            matched.push((name.clone(), reason));
        }
    }
    matched.sort_by(|a, b| a.0.cmp(&b.0));
    matched
}

fn detect_one(root: &Path, profile: &Profile) -> Option<MatchReason> {
    let d = &profile.detect;

    let root_slash = format!("{}/", root.display());
    if let Some(p) = d
        .path_prefixes
        .iter()
        .find(|p| !p.is_empty() && root_slash.starts_with(p.as_str()))
    {
        return Some(MatchReason {
            rule: "path_prefix",
            value: Some(p.clone()),
        });
    }

    if d.deps_keywords.is_empty() {
        let content_files: std::collections::BTreeSet<&str> =
            d.content.iter().map(|c| c.file.as_str()).collect();
        if let Some(f) = d
            .marker_files
            .iter()
            .find(|f| !f.is_empty() && !content_files.contains(f.as_str()) && root.join(f).exists())
        {
            return Some(MatchReason {
                rule: "marker_file",
                value: Some(f.clone()),
            });
        }
    }

    if let Some(g) = d
        .marker_globs
        .iter()
        .find(|g| !g.is_empty() && glob_exists(root, g))
    {
        return Some(MatchReason {
            rule: "marker_glob",
            value: Some(g.clone()),
        });
    }

    if let Some(cr) = d.content.iter().find(|cr| {
        !cr.file.is_empty()
            && !cr.word.is_empty()
            && std::fs::read_to_string(root.join(&cr.file))
                .map(|text| contains_word(&text, &cr.word))
                .unwrap_or(false)
    }) {
        return Some(MatchReason {
            rule: "content",
            value: Some(format!("{} → {}", cr.file, cr.word)),
        });
    }

    if !d.package_json_deps.is_empty() {
        if let Some(dep) = package_json_dep_hit(root, &d.package_json_deps) {
            return Some(MatchReason {
                rule: "package_json_dep",
                value: Some(dep),
            });
        }
    }

    if !d.deps_keywords.is_empty() {
        if let Some(kw) = deps_keyword_hit(root, &d.marker_files, &d.deps_keywords) {
            return Some(MatchReason {
                rule: "deps_keyword",
                value: Some(kw),
            });
        }
    }

    None
}

/// Match a file *name* against a glob supporting `*` and `?` (like find -name).
pub(crate) fn name_matches_glob(name: &str, pattern: &str) -> bool {
    fn helper(n: &[u8], p: &[u8]) -> bool {
        if p.is_empty() {
            return n.is_empty();
        }
        match p[0] {
            b'*' => helper(n, &p[1..]) || (!n.is_empty() && helper(&n[1..], p)),
            b'?' => !n.is_empty() && helper(&n[1..], &p[1..]),
            c => !n.is_empty() && n[0] == c && helper(&n[1..], &p[1..]),
        }
    }
    helper(name.as_bytes(), pattern.as_bytes())
}

/// Case-insensitive whole-word search: `(^|[^a-z0-9_])kw([^a-z0-9_]|$)`.
pub(crate) fn contains_word(haystack: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let h = haystack.to_ascii_lowercase();
    let w = word.to_ascii_lowercase();
    let hb = h.as_bytes();
    let wlen = w.len();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = h[start..].find(w.as_str()) {
        let i = start + pos;
        let before_ok = i == 0 || !is_word(hb[i - 1]);
        let after = i + wlen;
        let after_ok = after >= hb.len() || !is_word(hb[after]);
        if before_ok && after_ok {
            return true;
        }
        start = i + 1;
        if start >= hb.len() {
            break;
        }
    }
    false
}

/// Dirent budget for a single `globs_exist` walk: caps worst-case cost on huge
/// repos (the profile-view perf hot path) instead of walking unbounded.
pub(crate) const GLOB_WALK_BUDGET: usize = 200_000;

/// Answer whether each of `patterns` matches some file name in a single walk
/// of `root`, spending at most `budget` dirents. Returns the per-pattern hits
/// plus whether the budget ran out before every pattern was resolved —
/// patterns not yet matched at that point report `false`.
pub(crate) fn globs_exist(
    root: &Path,
    patterns: &[String],
    budget: usize,
) -> (BTreeMap<String, bool>, bool) {
    let mut hits: BTreeMap<String, bool> = patterns.iter().map(|p| (p.clone(), false)).collect();
    let mut remaining: std::collections::BTreeSet<&str> =
        patterns.iter().map(|p| p.as_str()).collect();
    if remaining.is_empty() {
        return (hits, false);
    }

    let mut budget_left = budget;
    let mut exhausted = false;
    let mut stack = vec![root.to_path_buf()];
    'walk: while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if budget_left == 0 {
                exhausted = true;
                break 'walk;
            }
            budget_left -= 1;

            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if ft.is_dir() {
                // Shared with repo enumeration so the two walks can't drift; see
                // `scan::is_pruned_dir` for why each name is on the list.
                if crate::profile::scan::is_pruned_dir(&name) {
                    continue;
                }
                stack.push(entry.path());
            } else if ft.is_file() {
                let matched: Vec<&str> = remaining
                    .iter()
                    .filter(|p| name_matches_glob(&name, p))
                    .copied()
                    .collect();
                for p in matched {
                    remaining.remove(p);
                    hits.insert(p.to_string(), true);
                }
                if remaining.is_empty() {
                    break 'walk;
                }
            }
        }
    }
    (hits, exhausted)
}

pub(crate) fn glob_exists(root: &Path, pattern: &str) -> bool {
    globs_exist(root, &[pattern.to_string()], GLOB_WALK_BUDGET).0[pattern]
}

fn package_json_dep_hit(root: &Path, deps: &[String]) -> Option<String> {
    let bytes = std::fs::read(root.join("package.json")).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let has = |obj: &str, d: &str| v.get(obj).and_then(|o| o.get(d)).is_some();
    deps.iter()
        .find(|d| has("dependencies", d) || has("devDependencies", d))
        .cloned()
}

fn deps_keyword_hit(root: &Path, marker_files: &[String], keywords: &[String]) -> Option<String> {
    let contents: Vec<String> = marker_files
        .iter()
        .filter(|f| !f.is_empty())
        .filter_map(|f| std::fs::read_to_string(root.join(f)).ok())
        .collect();
    keywords
        .iter()
        .filter(|k| !k.is_empty())
        .find(|kw| contents.iter().any(|text| contains_word(text, kw)))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profiles;

    fn cfg() -> Profiles {
        serde_json::from_str(r#"{
            "universal": [],
            "profiles": {
                "backend": {"plugins": [], "detect": {"marker_files": ["INFO"]}},
                "frontend": {"plugins": [], "detect": {"marker_globs": ["*.vue"], "package_json_deps": ["vue","react"]}},
                "plugin-dev": {"plugins": [], "detect": {"marker_files": ["plugin.json"]}},
                "ai-side": {"plugins": [], "detect": {"marker_files": ["requirements.txt"], "deps_keywords": ["openai","langchain"]}}
            }
        }"#).unwrap()
    }

    #[test]
    fn glob_matcher() {
        assert!(name_matches_glob("App.vue", "*.vue"));
        assert!(!name_matches_glob("App.js", "*.vue"));
        assert!(name_matches_glob("a.vue", "*.vue"));
        assert!(!name_matches_glob("vue", "*.vue"));
        assert!(name_matches_glob("x", "?"));
    }

    #[test]
    fn globs_exist_answers_all_patterns_in_one_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "x").unwrap();
        let pats = vec![
            "*.vue".to_string(),
            "*.svelte".to_string(),
            "*.rs".to_string(),
        ];
        let (hits, exhausted) = globs_exist(dir.path(), &pats, GLOB_WALK_BUDGET);
        assert!(!exhausted);
        assert!(hits["*.vue"]);
        assert!(!hits["*.svelte"]);
        assert!(hits["*.rs"]);
    }

    #[test]
    fn globs_exist_stops_at_budget_and_reports_exhaustion() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }
        // Budget smaller than the entry count: unmatched patterns report false + exhausted.
        let (hits, exhausted) = globs_exist(dir.path(), &["*.vue".to_string()], 5);
        assert!(exhausted);
        assert!(!hits["*.vue"]);
    }

    #[test]
    fn glob_exists_prunes_build_and_dependency_dirs() {
        // A non-matching glob must not trigger a full recursive walk of build /
        // dependency output — that walk (esp. Rust `target/`) is the profile-view
        // detect hot path. Generated files under these dirs must NOT count.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["target", "vendor", ".venv", "venv", "__pycache__", ".next"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
            std::fs::write(root.join(d).join("gen.rs"), "x").unwrap();
        }
        assert!(
            !glob_exists(root, "*.rs"),
            "build/dependency dirs must be pruned like node_modules/build/dist"
        );
        // A real top-level source file still matches.
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        assert!(glob_exists(root, "*.rs"), "real source still matches");
    }

    #[test]
    fn word_boundary() {
        assert!(contains_word("import openai\n", "openai"));
        assert!(contains_word("openai==1.2", "openai"));
        assert!(!contains_word("myopenaikey", "openai"));
        assert!(contains_word("LangChain rocks", "langchain"));
    }

    #[test]
    fn override_file_wins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude").join("profile"),
            "frontend\n# comment\n\nbackend\n",
        )
        .unwrap();
        // Written frontend-then-backend; read back sorted (see names.sort()).
        let got = detect_profiles(dir.path(), &cfg());
        assert_eq!(got, vec!["backend".to_string(), "frontend".to_string()]);
    }

    #[test]
    fn marker_file_detects_but_not_for_deps_keyword_profile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("INFO"), "x").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();
        let got = detect_profiles(dir.path(), &cfg());
        assert_eq!(got, vec!["backend".to_string()]);
    }

    #[test]
    fn deps_keyword_gates_ai_side() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "openai==1.0\n").unwrap();
        let got = detect_profiles(dir.path(), &cfg());
        assert_eq!(got, vec!["ai-side".to_string()]);
    }

    #[test]
    fn marker_glob_and_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        let got = detect_profiles(dir.path(), &cfg());
        assert_eq!(got, vec!["frontend".to_string()]);

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(
            dir2.path().join("package.json"),
            r#"{"dependencies":{"react":"^18"}}"#,
        )
        .unwrap();
        let got2 = detect_profiles(dir2.path(), &cfg());
        assert_eq!(got2, vec!["frontend".to_string()]);
    }

    #[test]
    fn glob_prunes_node_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/Thing.vue"), "x").unwrap();
        let got = detect_profiles(dir.path(), &cfg());
        assert!(got.is_empty());
    }

    #[test]
    fn path_prefix_match() {
        let dir = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(dir.path()).unwrap();
        let prefix = format!("{}/", canon.display());
        let mut c = Profiles::default();
        c.profiles.insert(
            "backend".into(),
            serde_json::from_value(serde_json::json!({
                "plugins": [], "detect": {"path_prefixes": [prefix]}
            }))
            .unwrap(),
        );
        let got = detect_profiles(dir.path(), &c);
        assert_eq!(got, vec!["backend".to_string()]);
    }

    #[test]
    fn detect_one_reports_marker_glob_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        let got = detect_profiles_explained(dir.path(), &cfg());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "frontend");
        assert_eq!(got[0].1.rule, "marker_glob");
        assert_eq!(got[0].1.value.as_deref(), Some("*.vue"));
    }

    #[test]
    fn detect_one_reports_marker_file_keyword_and_dep() {
        let d1 = tempfile::tempdir().unwrap();
        std::fs::write(d1.path().join("INFO"), "x").unwrap();
        let g1 = detect_profiles_explained(d1.path(), &cfg());
        assert_eq!(g1[0].1.rule, "marker_file");
        assert_eq!(g1[0].1.value.as_deref(), Some("INFO"));

        let d2 = tempfile::tempdir().unwrap();
        std::fs::write(d2.path().join("requirements.txt"), "openai==1.0\n").unwrap();
        let g2 = detect_profiles_explained(d2.path(), &cfg());
        assert_eq!(g2[0].0, "ai-side");
        assert_eq!(g2[0].1.rule, "deps_keyword");
        assert_eq!(g2[0].1.value.as_deref(), Some("openai"));

        let d3 = tempfile::tempdir().unwrap();
        std::fs::write(
            d3.path().join("package.json"),
            r#"{"dependencies":{"react":"^18"}}"#,
        )
        .unwrap();
        let g3 = detect_profiles_explained(d3.path(), &cfg());
        assert_eq!(g3[0].0, "frontend");
        assert_eq!(g3[0].1.rule, "package_json_dep");
        assert_eq!(g3[0].1.value.as_deref(), Some("react"));
    }

    #[test]
    fn override_file_yields_override_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude").join("profile"),
            "frontend\nbackend\n",
        )
        .unwrap();
        let got = detect_profiles_explained(dir.path(), &cfg());
        assert_eq!(
            got.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            vec!["backend", "frontend"]
        );
        assert!(got
            .iter()
            .all(|(_, r)| r.rule == "override" && r.value.is_none()));
    }

    #[test]
    fn no_match_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_profiles_explained(dir.path(), &cfg()).is_empty());
    }

    #[test]
    fn content_rule_matches_exact_file_and_word() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "torch==2.0\n").unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"ml":{"plugins":[],"detect":{"content":[{"file":"requirements.txt","word":"torch"}]}}}}"#,
        )
        .unwrap();
        assert_eq!(detect_profiles(dir.path(), &cfg), vec!["ml".to_string()]);
    }

    #[test]
    fn content_rules_do_not_cross_contaminate() {
        // requirements.txt contains "svelte" (not torch). The pairs are
        // (requirements.txt→torch) and (package.json→svelte); neither holds,
        // so the OLD cartesian-product bug (files×words) must NOT resurface.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "svelte\n").unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"x":{"plugins":[],"detect":{"content":[
                {"file":"requirements.txt","word":"torch"},
                {"file":"package.json","word":"svelte"}]}}}}"#,
        )
        .unwrap();
        assert!(
            detect_profiles(dir.path(), &cfg).is_empty(),
            "no exact (file,word) pair holds; must not match"
        );
    }

    #[test]
    fn content_does_not_hijack_marker_files() {
        // Both the marker_file AND the content rule are satisfiable: Cargo.toml exists
        // (satisfies marker_files) AND requirements.txt exists containing "torch"
        // (satisfies the content rule). The invariant under test is that marker_files
        // fires FIRST and is reported as the matched rule — not content. Without both
        // files present the content branch can never fire at all, making the test vacuous.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "torch==2.0\n").unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"rust":{"plugins":[],"detect":{
                "marker_files":["Cargo.toml"],
                "content":[{"file":"requirements.txt","word":"torch"}]}}}}"#,
        )
        .unwrap();
        let got = detect_profiles_explained(dir.path(), &cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "rust");
        assert_eq!(
            got[0].1.rule, "marker_file",
            "marker_files must fire before content even when the content rule is also satisfiable"
        );
    }

    #[test]
    fn marker_file_referenced_by_content_does_not_match_on_existence() {
        // marker_files AND content both name package.json. package.json exists
        // but does NOT contain "react", so the content rule must NOT match —
        // and marker_files must NOT short-circuit on mere existence.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"fe":{"plugins":[],"detect":{
                "marker_files":["package.json"],
                "content":[{"file":"package.json","word":"react"}]}}}}"#,
        )
        .unwrap();
        assert!(
            detect_profiles(dir.path(), &cfg).is_empty(),
            "package.json exists but lacks 'react'; neither marker_files (hijack) nor content may match"
        );

        // When it DOES contain react, it matches via content (not marker_file).
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"18"}}"#,
        )
        .unwrap();
        let got = detect_profiles_explained(dir.path(), &cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].1.rule, "content",
            "must match via content, not marker_file"
        );
    }

    #[test]
    fn legacy_deps_keywords_still_match() {
        // Backward-compat regression: old deps_keywords still detects.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "openai==1\n").unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"ai":{"plugins":[],"detect":{
                "marker_files":["requirements.txt"],"deps_keywords":["openai"]}}}}"#,
        )
        .unwrap();
        assert_eq!(detect_profiles(dir.path(), &cfg), vec!["ai".to_string()]);
    }

    #[test]
    fn marker_files_partial_overlap_with_content_fires_on_unreferenced_file() {
        // marker_files: [Cargo.toml, package.json]; content references package.json.
        // Cargo.toml exists (not content-referenced) -> must fire marker_file(Cargo.toml).
        // package.json exists but is content-referenced and lacks the word -> must
        // NOT short-circuit marker_files on existence.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"profiles":{"p":{"plugins":[],"detect":{
                "marker_files":["Cargo.toml","package.json"],
                "content":[{"file":"package.json","word":"react"}]}}}}"#,
        )
        .unwrap();
        let got = detect_profiles_explained(dir.path(), &cfg);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.rule, "marker_file");
        assert_eq!(
            got[0].1.value.as_deref(),
            Some("Cargo.toml"),
            "the non-content-referenced marker file fires; package.json is skipped"
        );
    }
}
