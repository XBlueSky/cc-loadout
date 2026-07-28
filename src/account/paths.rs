use std::path::{Path, PathBuf};

/// The live Claude Code credential + config file locations.
#[derive(Debug, Clone)]
pub struct ClaudePaths {
    pub credentials: PathBuf,
    pub main_config: PathBuf,
}

/// Resolve live paths. `config_dir_override` is `$CLAUDE_CONFIG_DIR` if set.
/// Default (no override) uses `~/.claude/.credentials.json` and `~/.claude.json`
/// (the latter aligns with cc-switch's default; the inner `~/.claude/.claude.json`
/// is intentionally NOT preferred — see `stale_inner_warning`).
pub fn resolve(home: &Path, config_dir_override: Option<&Path>) -> ClaudePaths {
    match config_dir_override {
        Some(dir) => {
            let credentials = dir.join(".credentials.json");
            let main_config = match (dir.parent(), dir.file_name()) {
                (Some(parent), Some(name)) => {
                    parent.join(format!("{}.json", name.to_string_lossy()))
                }
                _ => home.join(".claude.json"),
            };
            ClaudePaths {
                credentials,
                main_config,
            }
        }
        None => ClaudePaths {
            credentials: home.join(".claude").join(".credentials.json"),
            main_config: home.join(".claude.json"),
        },
    }
}

/// If an inner `~/.claude/.claude.json` exists with a DIFFERENT `oauthAccount`
/// email than the chosen `canonical` config, return a warning. This catches the
/// stale-legacy-file situation where blindly preferring the inner file would
/// write into a dead config and the active account would silently never change.
pub fn stale_inner_warning(home: &Path, canonical: &Path) -> Option<String> {
    let inner = home.join(".claude").join(".claude.json");
    if inner == *canonical || !inner.exists() {
        return None;
    }
    let email_of = |p: &Path| -> Option<String> {
        let bytes = std::fs::read(p).ok()?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        v.get("oauthAccount")?
            .get("emailAddress")?
            .as_str()
            .map(str::to_string)
    };
    let inner_email = email_of(&inner)?;
    let canon_email = email_of(canonical).unwrap_or_default();
    if inner_email != canon_email {
        Some(format!(
            "stale {} holds a different account ({}); writing canonical {} instead",
            inner.display(),
            inner_email,
            canonical.display()
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn resolves_default_home_layout() {
        let home = Path::new("/home/u");
        let p = resolve(home, None);
        assert_eq!(
            p.credentials,
            Path::new("/home/u/.claude/.credentials.json")
        );
        assert_eq!(p.main_config, Path::new("/home/u/.claude.json"));
    }

    #[test]
    fn resolves_config_dir_override() {
        let home = Path::new("/home/u");
        let p = resolve(home, Some(Path::new("/home/u/.claude-work")));
        assert_eq!(
            p.credentials,
            Path::new("/home/u/.claude-work/.credentials.json")
        );
        assert_eq!(p.main_config, Path::new("/home/u/.claude-work.json"));
    }

    #[test]
    fn stale_inner_file_with_other_email_warns() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"tony@x"}}"#,
        )
        .unwrap();
        std::fs::write(
            home.join(".claude").join(".claude.json"),
            br#"{"oauthAccount":{"emailAddress":"other@x"}}"#,
        )
        .unwrap();
        let canonical = home.join(".claude.json");
        let w = stale_inner_warning(home, &canonical);
        assert!(w.is_some(), "expected a warning");
        assert!(w.unwrap().contains("other@x"));
    }

    #[test]
    fn no_warning_when_inner_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let canonical = home.join(".claude.json");
        assert!(stale_inner_warning(home, &canonical).is_none());
    }
}
