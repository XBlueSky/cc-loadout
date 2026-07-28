use std::collections::BTreeSet;

use crate::profile::config::Profiles;

/// universal ∪ matched profiles' plugins (sorted, deduped).
pub fn desired_plugins(cfg: &Profiles, matched: &[String]) -> Vec<String> {
    let mut set: BTreeSet<String> = cfg.universal.iter().cloned().collect();
    for name in matched {
        if let Some(p) = cfg.profiles.get(name) {
            set.extend(p.plugins.iter().cloned());
        }
    }
    set.into_iter().collect()
}

/// universal ∪ ALL profiles' plugins — the full set of keys cc-loadout manages.
pub fn managed_keys(cfg: &Profiles) -> Vec<String> {
    let mut set: BTreeSet<String> = cfg.universal.iter().cloned().collect();
    for p in cfg.profiles.values() {
        set.extend(p.plugins.iter().cloned());
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::config::Profiles;

    fn cfg() -> Profiles {
        serde_json::from_str(
            r#"{
            "universal": ["u1@m", "u2@m"],
            "profiles": {
                "a": {"plugins": ["a1@m", "a2@m"], "detect": {}},
                "b": {"plugins": ["b1@m"], "detect": {}}
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn desired_is_universal_plus_matched() {
        let got = desired_plugins(&cfg(), &["a".to_string()]);
        assert_eq!(got, vec!["a1@m", "a2@m", "u1@m", "u2@m"]);
    }

    #[test]
    fn managed_is_universal_plus_all() {
        let got = managed_keys(&cfg());
        assert_eq!(got, vec!["a1@m", "a2@m", "b1@m", "u1@m", "u2@m"]);
    }
}
