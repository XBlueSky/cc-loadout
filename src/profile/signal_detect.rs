//! Canonical rule atoms + the index vocabulary. An *atom* is one disk question
//! a detect rule asks ("does any *.tsx exist?"); the scan answers every atom in
//! the vocabulary so interactive code never has to ask the disk itself.

use std::collections::BTreeSet;

use crate::profile::config::{Profile, Profiles};
use crate::profile::detect::MatchReason;
use crate::profile::discover::RepoSignal;

pub fn atom_glob(g: &str) -> String {
    format!("glob:{g}")
}
pub fn atom_file(f: &str) -> String {
    format!("file:{f}")
}
pub fn atom_content(file: &str, word: &str) -> String {
    format!("content:{file}\u{2192}{word}")
}
pub fn atom_kw(file: &str, kw: &str) -> String {
    format!("kw:{file}\u{2192}{kw}")
}

pub fn vocabulary(cfg: &Profiles) -> BTreeSet<String> {
    use crate::profile::discover::{KNOWN_GLOBS, MARKER_FILES};
    let mut v: BTreeSet<String> = MARKER_FILES.iter().map(|m| atom_file(m)).collect();
    v.extend(KNOWN_GLOBS.iter().map(|g| atom_glob(g)));
    for p in cfg.profiles.values() {
        let d = &p.detect;
        v.extend(
            d.marker_files
                .iter()
                .filter(|s| !s.is_empty())
                .map(|f| atom_file(f)),
        );
        v.extend(
            d.marker_globs
                .iter()
                .filter(|s| !s.is_empty())
                .map(|g| atom_glob(g)),
        );
        v.extend(
            d.content
                .iter()
                .filter(|c| !c.file.is_empty() && !c.word.is_empty())
                .map(|c| atom_content(&c.file, &c.word)),
        );
        for kw in d.deps_keywords.iter().filter(|k| !k.is_empty()) {
            for f in d.marker_files.iter().filter(|f| !f.is_empty()) {
                v.insert(atom_kw(f, kw));
            }
        }
    }
    v
}

/// Tri-state answer for one profile against one repo's indexed signal:
/// `Match` carries the same provenance `detect_one` would report, `Unknown`
/// means some atom the rules need was never indexed (the caller should treat
/// this as "ask the disk"), `NoMatch` is a definite miss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAnswer {
    Match(MatchReason),
    NoMatch,
    Unknown,
}

/// Evaluate one profile's detect rules against an indexed `RepoSignal`,
/// mirroring `detect::detect_one` rule-for-rule (path_prefix → marker_file →
/// marker_glob → content → package_json_deps → deps_keywords; first match
/// wins). Every gate (deps_keywords disables marker_file; content-referenced
/// files are excluded from marker_file; all the `!x.is_empty()` filters)
/// matches `detect_one` exactly — see detect.rs:65-137.
///
/// A missing atom (absent from `sig.rule_hits`) does not fail its rule
/// outright: it marks `saw_unknown` for that rule family and evaluation moves
/// on to the next rule, so a *later* rule can still produce a definite
/// `Match`. Provenance is therefore the first definite match, not necessarily
/// the first rule in list order — Unknown rules earlier in the order are
/// skipped over, exactly like disk detection would if the caller re-asked
/// with the missing atom resolved. Only when no rule ever matches AND at
/// least one atom was missing do we report `Unknown` instead of `NoMatch`.
pub fn profile_answer(sig: &RepoSignal, profile: &Profile) -> ProfileAnswer {
    let d = &profile.detect;
    let mut saw_unknown = false;
    let hit = |atom: &str| -> Option<bool> { sig.rule_hits.get(atom).copied() };

    let root_slash = format!("{}/", sig.path);
    if let Some(p) = d
        .path_prefixes
        .iter()
        .find(|p| !p.is_empty() && root_slash.starts_with(p.as_str()))
    {
        return ProfileAnswer::Match(MatchReason {
            rule: "path_prefix",
            value: Some(p.clone()),
        });
    }

    if d.deps_keywords.is_empty() {
        let content_files: std::collections::BTreeSet<&str> =
            d.content.iter().map(|c| c.file.as_str()).collect();
        for f in d
            .marker_files
            .iter()
            .filter(|f| !f.is_empty() && !content_files.contains(f.as_str()))
        {
            match hit(&atom_file(f)) {
                Some(true) => {
                    return ProfileAnswer::Match(MatchReason {
                        rule: "marker_file",
                        value: Some(f.clone()),
                    })
                }
                Some(false) => {}
                None => saw_unknown = true,
            }
        }
    }

    for g in d.marker_globs.iter().filter(|g| !g.is_empty()) {
        match hit(&atom_glob(g)) {
            Some(true) => {
                return ProfileAnswer::Match(MatchReason {
                    rule: "marker_glob",
                    value: Some(g.clone()),
                })
            }
            Some(false) => {}
            None => saw_unknown = true,
        }
    }

    for cr in d
        .content
        .iter()
        .filter(|c| !c.file.is_empty() && !c.word.is_empty())
    {
        match hit(&atom_content(&cr.file, &cr.word)) {
            Some(true) => {
                return ProfileAnswer::Match(MatchReason {
                    rule: "content",
                    value: Some(format!("{} → {}", cr.file, cr.word)),
                })
            }
            Some(false) => {}
            None => saw_unknown = true,
        }
    }

    if !d.package_json_deps.is_empty() {
        // package_json_deps is fully indexed (the scan stores the whole dep
        // list), so this rule is always answerable — never Unknown.
        if let Some(dep) = d
            .package_json_deps
            .iter()
            .find(|dep| sig.package_json_deps.iter().any(|have| have == *dep))
        {
            return ProfileAnswer::Match(MatchReason {
                rule: "package_json_dep",
                value: Some(dep.clone()),
            });
        }
    }

    if !d.deps_keywords.is_empty() {
        for kw in d.deps_keywords.iter().filter(|k| !k.is_empty()) {
            let mut any_unknown = false;
            for f in d.marker_files.iter().filter(|f| !f.is_empty()) {
                match hit(&atom_kw(f, kw)) {
                    Some(true) => {
                        return ProfileAnswer::Match(MatchReason {
                            rule: "deps_keyword",
                            value: Some(kw.clone()),
                        })
                    }
                    Some(false) => {}
                    None => any_unknown = true,
                }
            }
            if any_unknown {
                saw_unknown = true;
            }
        }
    }

    if saw_unknown {
        ProfileAnswer::Unknown
    } else {
        ProfileAnswer::NoMatch
    }
}

/// Repo-level detection from an indexed signal, WITH provenance — the
/// signal-driven analog of `detect::detect_profiles_explained`. An
/// `override_names` short-circuit reports `rule: "override", value: None"`
/// for every name, exactly like the disk path. Otherwise every profile in
/// `cfg.profiles` (a `BTreeMap`, already name-sorted — matching
/// `detect_profiles_explained`'s explicit sort at detect.rs:60) is evaluated;
/// `.1` is `true` if *any* profile answered `Unknown`, telling the caller this
/// repo's rendering is provisional pending a full atom index.
pub fn detect_from_signal_explained(
    sig: &RepoSignal,
    cfg: &Profiles,
) -> (Vec<(String, MatchReason)>, bool) {
    if let Some(names) = &sig.override_names {
        return (
            names
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        MatchReason {
                            rule: "override",
                            value: None,
                        },
                    )
                })
                .collect(),
            false,
        );
    }

    let mut matched = Vec::new();
    let mut pending = false;
    for (name, profile) in &cfg.profiles {
        match profile_answer(sig, profile) {
            ProfileAnswer::Match(reason) => matched.push((name.clone(), reason)),
            ProfileAnswer::Unknown => pending = true,
            ProfileAnswer::NoMatch => {}
        }
    }
    (matched, pending)
}

/// Thin name-only view over `detect_from_signal_explained`, mirroring
/// `detect::detect_profiles` over `detect_profiles_explained`.
pub fn detect_from_signal(sig: &RepoSignal, cfg: &Profiles) -> (Vec<String>, bool) {
    let (explained, pending) = detect_from_signal_explained(sig, cfg);
    (
        explained.into_iter().map(|(name, _)| name).collect(),
        pending,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig_with(hits: &[(&str, bool)]) -> RepoSignal {
        RepoSignal {
            path: "/x".into(),
            marker_files: vec![],
            marker_globs: vec![],
            package_json_deps: vec![],
            languages: vec![],
            rule_hits: hits.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            override_names: None,
        }
    }

    #[test]
    fn mirrors_detect_one_rule_order_and_gates() {
        // marker_file is gated off when deps_keywords is non-empty (detect.rs:79),
        // and content-referenced files never count as marker files (detect.rs:85).
        let sig = sig_with(&[
            ("file:requirements.txt", true),
            ("kw:requirements.txt→openai", false),
        ]);
        let p: Profile = serde_json::from_str(
            r#"{"plugins": [], "detect": {"marker_files": ["requirements.txt"], "deps_keywords": ["openai"]}}"#,
        ).unwrap();
        // marker file exists but is gated; kw missed → NoMatch, NOT Match.
        assert!(matches!(profile_answer(&sig, &p), ProfileAnswer::NoMatch));
    }

    #[test]
    fn unknown_atom_yields_unknown_not_nomatch() {
        let sig = sig_with(&[]); // empty index
        let p: Profile =
            serde_json::from_str(r#"{"plugins": [], "detect": {"marker_globs": ["*.tsx"]}}"#)
                .unwrap();
        assert!(matches!(profile_answer(&sig, &p), ProfileAnswer::Unknown));
    }

    #[test]
    fn parity_with_disk_detect_when_fully_indexed() {
        // Build a real fixture repo, scan it with vocabulary(cfg), and require the
        // signal evaluator to agree with detect_profiles_explained on names.
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("r");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();
        std::fs::write(repo.join("App.vue"), "x").unwrap();
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal": [], "profiles": {
            "rust": {"plugins": [], "detect": {"marker_files": ["Cargo.toml"]}},
            "frontend": {"plugins": [], "detect": {"marker_globs": ["*.vue"]}},
            "go": {"plugins": [], "detect": {"marker_files": ["go.mod"]}}}}"#,
        )
        .unwrap();
        let vocab = vocabulary(&cfg);
        let sigs = crate::profile::discover::scan_repo_signals(
            &[dir.path().display().to_string()],
            6,
            &vocab,
        );
        let (names, pending) = detect_from_signal(&sigs[0], &cfg);
        assert!(!pending);
        let disk: Vec<String> = crate::profile::detect::detect_profiles(&repo, &cfg);
        assert_eq!(names, disk);
    }

    #[test]
    fn parity_with_disk_detect_through_symlinked_scan_root() {
        // scan_repo_signals must canonicalize repo paths at scan time
        // (mirroring detect_profiles_explained's canonicalize-before-match,
        // detect.rs:27-28), so path_prefix rules still agree with disk
        // detection when the scan root is reached via a symlink.
        // RepoSignal.path must NOT retain symlink components — see
        // detect.rs's own `path_prefix_match` test (detect.rs:436-449) for
        // the same trap on the disk side.
        let real = tempfile::tempdir().unwrap();
        let repo = real.path().join("r");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let link_dir = tempfile::tempdir().unwrap();
        let link_root = link_dir.path().join("via_link");
        std::os::unix::fs::symlink(real.path(), &link_root).unwrap();

        // The configured prefix is naturally canonical (as real
        // path_prefixes are in practice), built from the real (non-symlink)
        // repo path — NOT from the symlinked root we're about to scan.
        let canon_repo = std::fs::canonicalize(&repo).unwrap();
        let prefix = format!("{}/", canon_repo.display());
        let mut cfg = Profiles::default();
        cfg.profiles.insert(
            "backend".into(),
            serde_json::from_value(serde_json::json!({
                "plugins": [], "detect": {"path_prefixes": [prefix]}
            }))
            .unwrap(),
        );

        let vocab = vocabulary(&cfg);
        let sigs = crate::profile::discover::scan_repo_signals(
            &[link_root.display().to_string()],
            6,
            &vocab,
        );
        assert_eq!(sigs.len(), 1);
        let (names, pending) = detect_from_signal(&sigs[0], &cfg);
        assert!(!pending);

        // Disk detection on the SAME symlinked path must agree — both sides
        // resolve to the canonical form before matching path_prefixes.
        let via_link_repo = link_root.join("r");
        let disk: Vec<String> = crate::profile::detect::detect_profiles(&via_link_repo, &cfg);
        assert_eq!(names, disk);
        assert_eq!(names, vec!["backend".to_string()]);
    }

    fn cfg() -> Profiles {
        serde_json::from_str(
            r#"{
            "universal": [],
            "profiles": {
                "frontend": {"plugins": [], "detect": {"marker_globs": ["*.vue", "*.svelte"]}},
                "web": {"plugins": [], "detect": {"marker_files": ["web/Cargo.toml"],
                        "content": [{"file": ".git/config", "word": "git.synology.inc"}]}},
                "ai": {"plugins": [], "detect": {"marker_files": ["requirements.txt"],
                        "deps_keywords": ["openai"]}}
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn vocabulary_covers_builtins_and_rule_atoms() {
        let v = vocabulary(&cfg());
        // built-ins (from discover)
        assert!(v.contains(&atom_file("go.mod")), "built-in marker file");
        assert!(v.contains(&atom_glob("*.vue")), "built-in glob");
        // rule-referenced atoms
        assert!(v.contains(&atom_glob("*.svelte")));
        assert!(v.contains(&atom_file("web/Cargo.toml")));
        assert!(v.contains(&atom_content(".git/config", "git.synology.inc")));
        assert!(v.contains(&atom_kw("requirements.txt", "openai")));
    }

    #[test]
    fn vocabulary_skips_empty_strings() {
        let cfg: Profiles = serde_json::from_str(
            r#"{"universal": [], "profiles": {
                "x": {"plugins": [], "detect": {"marker_files": [""], "marker_globs": [""]}}}}"#,
        )
        .unwrap();
        let v = vocabulary(&cfg);
        assert!(!v.contains(&atom_file("")));
        assert!(!v.contains(&atom_glob("")));
    }
}
