use anyhow::Result;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

use crate::profile::apply;
use crate::profile::config::Profiles;
use crate::profile::detect::{detect_profiles, detect_profiles_explained};
use crate::profile::plugins::desired_plugins;

/// Uniform wrapper: `--json` for detect/apply/status is always `{repos:[…]}`.
#[derive(Serialize)]
pub struct ReposJson<T: Serialize> {
    pub repos: Vec<T>,
}

#[derive(Serialize)]
pub struct SignalJson {
    pub profile: String,
    pub rule: &'static str,
    pub value: Option<String>,
}

#[derive(Serialize)]
pub struct DetectRepoJson {
    pub repo: String,
    pub matched: Vec<String>,
    pub plugins: Vec<String>,
    pub signals: Vec<SignalJson>,
}

#[derive(Serialize)]
pub struct ChangeJson {
    pub plugin: String,
    pub from: Option<bool>,
    pub to: Option<bool>,
}

#[derive(Serialize)]
pub struct ApplyRepoJson {
    pub repo: String,
    pub matched: Vec<String>,
    pub changed: Vec<ChangeJson>,
}

#[derive(Serialize)]
pub struct StatusRepoJson {
    pub repo: String,
    pub applied: Vec<String>,
}

/// detect: matched profiles + the resulting enabled plugin set (read-only).
pub fn detect_repo_json(repo: &Path, cfg: &Profiles) -> DetectRepoJson {
    let explained = detect_profiles_explained(repo, cfg);
    let matched: Vec<String> = explained.iter().map(|(n, _)| n.clone()).collect();
    let plugins = desired_plugins(cfg, &matched);
    let signals = explained
        .into_iter()
        .map(|(profile, r)| SignalJson {
            profile,
            rule: r.rule,
            value: r.value,
        })
        .collect();
    DetectRepoJson {
        repo: repo.display().to_string(),
        matched,
        plugins,
        signals,
    }
}

/// apply: applies (writes settings.local.json) and reports the diff. When
/// `dry_run`, computes the same diff via [`apply::preview`] without writing —
/// so `apply --all --dry-run --json` lists exactly the out-of-sync repos.
pub fn apply_repo_json(repo: &Path, cfg: &Profiles, dry_run: bool) -> Result<ApplyRepoJson> {
    let matched = detect_profiles(repo, cfg);
    let (before, after) = if dry_run {
        apply::preview(repo, cfg, &matched)?
    } else {
        apply::apply(repo, cfg, &matched)?
    };
    Ok(ApplyRepoJson {
        repo: repo.display().to_string(),
        matched,
        changed: diff(&before, &after),
    })
}

/// status: the enabled plugin keys currently in settings.local.json.
pub fn status_repo_json(repo: &Path) -> Result<StatusRepoJson> {
    Ok(StatusRepoJson {
        repo: repo.display().to_string(),
        applied: apply::enabled_keys(repo)?,
    })
}

/// Plugin keys whose enabled state differs between before/after (sorted).
/// `from`/`to` are `bool|null` (null = key absent or non-boolean on that side).
fn diff(before: &Value, after: &Value) -> Vec<ChangeJson> {
    let empty = Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);
    let mut keys: BTreeSet<&String> = a.keys().collect();
    keys.extend(b.keys());
    keys.into_iter()
        .filter_map(|k| {
            let from = b.get(k).and_then(Value::as_bool);
            let to = a.get(k).and_then(Value::as_bool);
            if from != to {
                Some(ChangeJson {
                    plugin: k.clone(),
                    from,
                    to,
                })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Profiles {
        serde_json::from_str(
            r#"{
            "universal": ["u@m"],
            "profiles": {
                "frontend": {"plugins": ["fe@m"], "detect": {"marker_globs": ["*.vue"]}},
                "backend": {"plugins": ["be@m"], "detect": {"marker_files": ["INFO"]}}
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn detect_repo_json_matches_and_lists_plugins() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        let j = detect_repo_json(dir.path(), &cfg());
        assert_eq!(j.matched, vec!["frontend"]);
        assert!(j.plugins.contains(&"fe@m".to_string()));
        assert!(j.plugins.contains(&"u@m".to_string()));
        assert!(!j.plugins.contains(&"be@m".to_string()));
        assert_eq!(j.signals.len(), 1);
        assert_eq!(j.signals[0].profile, "frontend");
        assert_eq!(j.signals[0].rule, "marker_glob");
        assert_eq!(j.signals[0].value.as_deref(), Some("*.vue"));
    }

    #[test]
    fn detect_repo_json_no_match_has_empty_signals() {
        let dir = tempfile::tempdir().unwrap();
        let j = detect_repo_json(dir.path(), &cfg());
        assert!(j.matched.is_empty());
        assert!(j.signals.is_empty());
    }

    #[test]
    fn apply_repo_json_reports_changed_then_empty_on_rerun() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        let j = apply_repo_json(dir.path(), &cfg(), false).unwrap();
        let fe = j.changed.iter().find(|c| c.plugin == "fe@m").unwrap();
        assert_eq!(fe.from, None);
        assert_eq!(fe.to, Some(true));
        let j2 = apply_repo_json(dir.path(), &cfg(), false).unwrap();
        assert!(j2.changed.is_empty());
    }

    #[test]
    fn status_repo_json_lists_enabled_after_apply() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("App.vue"), "x").unwrap();
        assert!(status_repo_json(dir.path()).unwrap().applied.is_empty());
        apply_repo_json(dir.path(), &cfg(), false).unwrap();
        let s = status_repo_json(dir.path()).unwrap();
        assert!(s.applied.contains(&"fe@m".to_string()));
        assert!(s.applied.contains(&"u@m".to_string()));
        assert!(!s.applied.contains(&"be@m".to_string()));
    }

    #[test]
    fn repos_envelope_is_flat() {
        let payload = ReposJson {
            repos: vec![DetectRepoJson {
                repo: "/r".into(),
                matched: vec![],
                plugins: vec![],
                signals: vec![],
            }],
        };
        let s = crate::json::to_string(&payload).unwrap();
        assert!(s.contains("\"schema_version\": 1"));
        assert!(s.contains("\"repos\""));
    }
}
