use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Validate and normalise one `"HH:MM"` (24h). Returns the zero-padded form.
pub fn parse_hhmm(s: &str) -> Result<String> {
    let t = s.trim();
    let (h, m) = t
        .split_once(':')
        .with_context(|| format!("invalid time '{t}': expected HH:MM"))?;
    let h: u32 = h
        .trim()
        .parse()
        .with_context(|| format!("invalid hour in '{t}'"))?;
    let m: u32 = m
        .trim()
        .parse()
        .with_context(|| format!("invalid minute in '{t}'"))?;
    if h > 23 || m > 59 {
        bail!("invalid time '{t}': hour must be 0-23, minute 0-59");
    }
    Ok(format!("{h:02}:{m:02}"))
}

/// Parse a free-form list ("06:00, 11:00 16:00") into sorted, deduped, validated
/// times. Empty input yields an empty list (used to clear an alias's schedule).
pub fn parse_times(input: &str) -> Result<Vec<String>> {
    let mut set = std::collections::BTreeSet::new();
    for tok in input.split([',', ' ', '\t', '\n']) {
        if tok.trim().is_empty() {
            continue;
        }
        set.insert(parse_hhmm(tok)?);
    }
    Ok(set.into_iter().collect())
}

/// Schema version of `tasks.json`. Fail-fast on a newer version (mirrors state.json).
pub const TASKS_VERSION: u32 = 1;
fn default_tasks_version() -> u32 {
    TASKS_VERSION
}

/// Model a prime ping runs on. A prime exists only to open the 5-hour usage
/// window — its prompt is a throwaway "ok", so it has no business burning the
/// account's default (currently the top tier). Forced for every `kind: prime`
/// run unless that task pins an explicit `model`.
pub const PING_MODEL: &str = "haiku";

/// A prime fires a cheap ping to anchor a window; a task runs a real prompt and
/// persists a resumable session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Prime,
    Task,
}

/// One scheduled run definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDef {
    pub kind: Kind,
    /// Required: the account this run executes as.
    pub account: String,
    /// Daily fire times, "HH:MM", sorted/deduped/validated.
    pub times: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Model this run passes to `claude --model`. `None` ⇒ inherit whatever the
    /// CLI resolves by itself (settings.json / account default), except for
    /// primes, which fall back to [`PING_MODEL`]. See [`TaskDef::effective_model`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Session id of the most recent SUCCESSFUL run, for `task resume`. A failed
    /// run updates only `last_status`/`last_run` and leaves this pointing at the
    /// last good run (so you can still resume the last working session).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<String>,
    /// CLAUDE_CONFIG_DIR of the most recent successful run (`None` ⇒ live ~/.claude),
    /// paired with `last_session_id` so resume opens the right session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_config_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
}

impl TaskDef {
    /// The `--model` this run should pass, if any:
    ///
    /// 1. an explicit, non-blank `model` — always wins, primes included;
    /// 2. otherwise [`PING_MODEL`] for a prime, whose prompt is a throwaway ping;
    /// 3. otherwise `None` — let the CLI resolve its own default.
    pub fn effective_model(&self) -> Option<&str> {
        match self.model.as_deref().map(str::trim) {
            Some(m) if !m.is_empty() => Some(m),
            _ if self.kind == Kind::Prime => Some(PING_MODEL),
            _ => None,
        }
    }

    /// Static well-formedness: tasks need a prompt + cwd; primes need neither;
    /// every entry needs at least one time.
    pub fn validate(&self) -> Result<()> {
        if self.times.is_empty() {
            bail!("no times given");
        }
        // A model reaches `claude` as its own argv word, so anything that could
        // be read as another flag — or as several words — is rejected here
        // rather than silently changing what the scheduled run does.
        if let Some(m) = self.model.as_deref().map(str::trim) {
            if !m.is_empty() && (m.starts_with('-') || m.split_whitespace().count() > 1) {
                bail!("invalid model '{m}': expected a single alias or model name (e.g. haiku, claude-sonnet-4-6)");
            }
        }
        if self.kind == Kind::Task {
            if self.prompt.as_deref().unwrap_or("").trim().is_empty() {
                bail!("a task needs a --prompt");
            }
            if self.cwd.is_none() {
                bail!("a task needs a --cwd");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tasks {
    #[serde(default = "default_tasks_version")]
    pub version: u32,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskDef>,
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks {
            version: TASKS_VERSION,
            tasks: BTreeMap::new(),
        }
    }
}

/// `<data_root>/tasks.json`.
pub fn tasks_path(data_root: &Path) -> PathBuf {
    data_root.join("tasks.json")
}

pub fn load(path: &Path) -> Result<Tasks> {
    if !path.exists() {
        return Ok(Tasks::default());
    }
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let t: Tasks =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    if t.version > TASKS_VERSION {
        bail!(
            "{} has schema version {} which is newer than this cc-loadout understands (max {}); upgrade cc-loadout",
            path.display(),
            t.version,
            TASKS_VERSION
        );
    }
    Ok(t)
}

pub fn save(path: &Path, t: &Tasks) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(t)?;
    crate::util::atomicfile::write_atomic(path, &bytes, 0o600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hhmm_normalises_and_validates() {
        assert_eq!(parse_hhmm("6:00").unwrap(), "06:00");
        assert_eq!(parse_hhmm("23:59").unwrap(), "23:59");
        assert_eq!(parse_hhmm(" 09:5 ").unwrap(), "09:05");
        assert!(parse_hhmm("24:00").is_err());
        assert!(parse_hhmm("12:60").is_err());
        assert!(parse_hhmm("noon").is_err());
        assert!(parse_hhmm("12").is_err());
    }

    #[test]
    fn parse_times_sorts_dedupes_and_validates() {
        assert_eq!(
            parse_times("11:00, 06:00 21:00,16:00, 11:00").unwrap(),
            vec!["06:00", "11:00", "16:00", "21:00"]
        );
        assert_eq!(parse_times("   ").unwrap(), Vec::<String>::new());
        assert!(parse_times("06:00, bogus").is_err());
    }

    fn task_entry() -> TaskDef {
        TaskDef {
            kind: Kind::Task,
            account: "work".into(),
            times: vec!["07:00".into()],
            prompt: Some("/cortex:weekly".into()),
            cwd: Some(PathBuf::from("/workspace/cortex")),
            profile: Some("cortex".into()),
            model: None,
            last_session_id: None,
            last_config_dir: None,
            last_run: None,
            last_status: None,
        }
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        let mut t = Tasks::default();
        t.tasks.insert("weekly".into(), task_entry());
        save(&p, &t).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.version, TASKS_VERSION);
        assert_eq!(back.tasks["weekly"].account, "work");
        assert_eq!(
            back.tasks["weekly"].prompt.as_deref(),
            Some("/cortex:weekly")
        );
    }

    #[test]
    fn missing_file_loads_default() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&tasks_path(dir.path())).unwrap().tasks.is_empty());
    }

    #[test]
    fn newer_version_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        std::fs::write(&p, br#"{"version":999,"tasks":{}}"#).unwrap();
        assert!(load(&p).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn validate_task_requires_prompt_and_cwd() {
        let mut e = task_entry();
        e.prompt = None;
        assert!(e.validate().unwrap_err().to_string().contains("prompt"));
        let mut e = task_entry();
        e.cwd = None;
        assert!(e.validate().unwrap_err().to_string().contains("cwd"));
    }

    #[test]
    fn validate_prime_needs_neither_prompt_nor_cwd() {
        let e = TaskDef {
            kind: Kind::Prime,
            account: "work".into(),
            times: vec!["06:00".into()],
            prompt: None,
            cwd: None,
            profile: None,
            model: None,
            last_session_id: None,
            last_config_dir: None,
            last_run: None,
            last_status: None,
        };
        assert!(e.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_times() {
        let mut e = task_entry();
        e.times.clear();
        assert!(e.validate().unwrap_err().to_string().contains("no times"));
    }

    #[test]
    fn task_without_a_model_inherits_the_cli_default() {
        assert_eq!(task_entry().effective_model(), None);
    }

    #[test]
    fn prime_without_a_model_forces_the_cheap_ping_model() {
        let mut e = task_entry();
        e.kind = Kind::Prime;
        assert_eq!(e.effective_model(), Some(PING_MODEL));
    }

    #[test]
    fn explicit_model_wins_over_the_prime_default() {
        let mut e = task_entry();
        e.kind = Kind::Prime;
        e.model = Some("opus".into());
        assert_eq!(e.effective_model(), Some("opus"));
    }

    #[test]
    fn blank_model_is_treated_as_unset() {
        let mut e = task_entry();
        e.model = Some("   ".into());
        assert_eq!(e.effective_model(), None);
    }

    #[test]
    fn validate_rejects_a_flag_like_model() {
        let mut e = task_entry();
        e.model = Some("--dangerously-skip-permissions".into());
        assert!(e.validate().unwrap_err().to_string().contains("model"));
    }

    #[test]
    fn validate_rejects_a_model_with_whitespace() {
        let mut e = task_entry();
        e.model = Some("claude sonnet".into());
        assert!(e.validate().unwrap_err().to_string().contains("model"));
    }

    #[test]
    fn model_roundtrips_through_tasks_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        let mut t = Tasks::default();
        let mut e = task_entry();
        e.model = Some("haiku".into());
        t.tasks.insert("weekly".into(), e);
        save(&p, &t).unwrap();
        assert_eq!(
            load(&p).unwrap().tasks["weekly"].model.as_deref(),
            Some("haiku")
        );
    }
}
