//! Canonical rule atoms + the index vocabulary. An *atom* is one disk question
//! a detect rule asks ("does any *.tsx exist?"); the scan answers every atom in
//! the vocabulary so interactive code never has to ask the disk itself.

use std::collections::BTreeSet;

use crate::profile::config::Profiles;

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

#[cfg(test)]
mod tests {
    use super::*;

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
