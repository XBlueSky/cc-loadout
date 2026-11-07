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

/// Schema version `save` writes for a file with no collector content — and the
/// version a fresh file starts at. A file nobody has put a collector into stays
/// byte-identical to what an older cc-loadout wrote.
pub const TASKS_VERSION: u32 = 1;
/// Highest schema version `load` accepts (fail-fast on a newer one, mirrors
/// state.json). Version 2 is a file that carries collector content: an older
/// binary refuses it up front instead of silently dropping the keys it does not
/// know on its next save.
pub const TASKS_VERSION_MAX: u32 = 2;
fn default_tasks_version() -> u32 {
    TASKS_VERSION
}

/// Accepted `grace_ms` of a collector task. The floor is `KILL_WAIT_MS` (2000)
/// plus the ~500 ms shutdown window of the Claude Code process the launcher
/// supervises: a smaller grace can never leave the launcher time to finish.
pub const COLLECTOR_GRACE_MS: std::ops::RangeInclusive<u64> = 2500..=600_000;
/// Accepted `wall_timeout_secs` of a collector task (1 second to one day).
pub const COLLECTOR_WALL_TIMEOUT_SECS: std::ops::RangeInclusive<u64> = 1..=86_400;

/// Model a prime ping runs on. A prime exists only to open the 5-hour usage
/// window — its prompt is a throwaway "ok", so it has no business burning the
/// account's default (currently the top tier). Forced for every `kind: prime`
/// run unless that task pins an explicit `model`.
pub const PING_MODEL: &str = "haiku";

/// A prime fires a cheap ping to anchor a window; a task runs a real prompt and
/// persists a resumable session; a collector hands the run to Geralt's launcher
/// and keeps no session of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Prime,
    Task,
    Collector,
}

/// The per-task half of a collector: which launcher config to run and how long
/// it may take. Everything machine-wide lives in [`CollectorSettings`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectorTask {
    /// Absolute path of the launcher config the collector launcher is given.
    pub launcher_config: PathBuf,
    /// Hard wall-clock limit of one attempt, in seconds ([`COLLECTOR_WALL_TIMEOUT_SECS`]).
    pub wall_timeout_secs: u64,
    /// Time the launcher gets to wind down after the limit, in milliseconds
    /// ([`COLLECTOR_GRACE_MS`]).
    pub grace_ms: u64,
}

/// The machine-wide half of a collector, stored once at the top level of
/// `tasks.json` and shared by every collector task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectorSettings {
    /// Absolute path of the Node.js binary that runs the launcher.
    pub node: PathBuf,
    /// Absolute path of the Geralt checkout that holds the launcher.
    pub geralt_repo: PathBuf,
    /// Absolute path of the directory run records are exported to.
    pub export_dir: PathBuf,
    /// Identifies this runner in every exported record ([`is_collector_id`]).
    pub runner_instance_id: String,
}

/// Whether `s` is acceptable as a collector `taskId` or `runnerInstanceId`:
/// `^[A-Za-z0-9][A-Za-z0-9_.:-]{0,199}$`. Hand-rolled so the grammar is one
/// ASCII byte test, with no regex dependency and no Unicode whitespace question.
pub fn is_collector_id(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 200 || !b[0].is_ascii_alphanumeric() {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

impl CollectorSettings {
    /// Static well-formedness: the three paths are absolute and the runner
    /// instance id fits [`is_collector_id`].
    pub fn validate(&self) -> Result<()> {
        for (name, path) in [
            ("node", &self.node),
            ("geralt_repo", &self.geralt_repo),
            ("export_dir", &self.export_dir),
        ] {
            if !path.is_absolute() {
                bail!("collector {name} must be an absolute path");
            }
        }
        if !is_collector_id(&self.runner_instance_id) {
            bail!(
                "invalid collector runner_instance_id: expected 1-200 characters of \
                 letters, digits, '_', '.', ':' or '-', starting with a letter or digit"
            );
        }
        Ok(())
    }
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
    /// Present exactly when `kind` is [`Kind::Collector`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector: Option<CollectorTask>,
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
    /// 0. a collector never passes one — the launcher owns how Claude is started;
    /// 1. an explicit, non-blank `model` — always wins, primes included;
    /// 2. otherwise [`PING_MODEL`] for a prime, whose prompt is a throwaway ping;
    /// 3. otherwise `None` — let the CLI resolve its own default.
    pub fn effective_model(&self) -> Option<&str> {
        if self.kind == Kind::Collector {
            return None;
        }
        match self.model.as_deref().map(str::trim) {
            Some(m) if !m.is_empty() => Some(m),
            _ if self.kind == Kind::Prime => Some(PING_MODEL),
            _ => None,
        }
    }

    /// Static well-formedness: tasks need a prompt + cwd; primes need neither;
    /// a collector needs its `collector` struct and nothing a prompt run has;
    /// every entry needs at least one time.
    pub fn validate(&self) -> Result<()> {
        if self.times.is_empty() {
            bail!("no times given");
        }
        // Checked before the model rules below so a collector with a stray
        // `model` is told it cannot have one, not that the model looks odd.
        if self.kind == Kind::Collector {
            self.validate_collector()?;
        } else if self.collector.is_some() {
            bail!("only a collector task can carry a collector section");
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

    fn validate_collector(&self) -> Result<()> {
        let Some(c) = &self.collector else {
            bail!("a collector task needs its collector section");
        };
        for (name, set) in [
            ("prompt", self.prompt.is_some()),
            ("cwd", self.cwd.is_some()),
            ("profile", self.profile.is_some()),
            ("model", self.model.is_some()),
        ] {
            if set {
                bail!("a collector task cannot have a {name}");
            }
        }
        if !c.launcher_config.is_absolute() {
            bail!("collector launcher_config must be an absolute path");
        }
        if !COLLECTOR_WALL_TIMEOUT_SECS.contains(&c.wall_timeout_secs) {
            bail!(
                "collector wall_timeout_secs {} is out of range ({}..={})",
                c.wall_timeout_secs,
                COLLECTOR_WALL_TIMEOUT_SECS.start(),
                COLLECTOR_WALL_TIMEOUT_SECS.end()
            );
        }
        if !COLLECTOR_GRACE_MS.contains(&c.grace_ms) {
            bail!(
                "collector grace_ms {} is out of range ({}..={})",
                c.grace_ms,
                COLLECTOR_GRACE_MS.start(),
                COLLECTOR_GRACE_MS.end()
            );
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
    #[serde(default)]
    pub maintenance: Maintenance,
    /// Machine-wide collector settings, shared by every `kind: collector` task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector: Option<CollectorSettings>,
}

impl Tasks {
    /// The schema version this content needs on disk: [`TASKS_VERSION_MAX`] as
    /// soon as there is a collector section or any collector task, otherwise
    /// [`TASKS_VERSION`]. `save` writes this, not the in-memory `version`.
    pub fn schema_version(&self) -> u32 {
        if self.collector.is_some() || self.tasks.values().any(|d| d.kind == Kind::Collector) {
            TASKS_VERSION_MAX
        } else {
            TASKS_VERSION
        }
    }
}

/// The recurring housekeeping sweep. Unlike a [`TaskDef`] it runs no prompt and
/// consumes no account: it re-invokes this binary's own `doctor --fix
/// --prune-records`, so it needs neither a model, a session, nor a cwd. That is
/// why it is a field of its own rather than a third [`Kind`] — a `TaskDef`
/// requires an account, and a maintenance sweep has none to spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maintenance {
    #[serde(default)]
    pub enabled: bool,
    /// Daily fire times, "HH:MM".
    #[serde(default)]
    pub times: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
}

impl Default for Tasks {
    fn default() -> Self {
        Tasks {
            version: TASKS_VERSION,
            tasks: BTreeMap::new(),
            maintenance: Maintenance::default(),
            collector: None,
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
    if t.version > TASKS_VERSION_MAX {
        bail!(
            "{} has schema version {} which is newer than this cc-loadout understands (max {}); upgrade cc-loadout",
            path.display(),
            t.version,
            TASKS_VERSION_MAX
        );
    }
    Ok(t)
}

pub fn save(path: &Path, t: &Tasks) -> Result<()> {
    let mut out = t.clone();
    out.version = t.schema_version();
    let bytes = serde_json::to_vec_pretty(&out)?;
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
            collector: None,
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

    /// `tasks.json` exactly as cc-loadout 0.1.29's `save` writes it (pretty JSON,
    /// no trailing newline): a task, a prime and an enabled maintenance sweep.
    const V1_FIXTURE: &str = r#"{
  "version": 1,
  "tasks": {
    "weekly": {
      "kind": "task",
      "account": "work",
      "times": [
        "07:00"
      ],
      "prompt": "/cortex:weekly",
      "cwd": "/workspace/cortex",
      "profile": "cortex",
      "model": "claude-sonnet-4-6",
      "last_session_id": "dead-beef",
      "last_config_dir": "/data/accounts/work/run/cfg",
      "last_run": 1700,
      "last_status": "ok"
    },
    "work": {
      "kind": "prime",
      "account": "work",
      "times": [
        "06:00",
        "11:00"
      ]
    }
  },
  "maintenance": {
    "enabled": true,
    "times": [
      "04:00"
    ],
    "last_run": 1700,
    "last_status": "ok"
  }
}"#;

    #[test]
    fn v1_file_without_collector_round_trips_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        std::fs::write(&p, V1_FIXTURE).unwrap();
        let t = load(&p).unwrap();
        assert_eq!(t.version, 1);
        save(&p, &t).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), V1_FIXTURE);
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
            collector: None,
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

    fn collector_entry() -> TaskDef {
        TaskDef {
            kind: Kind::Collector,
            account: "work".into(),
            times: vec!["07:00".into()],
            prompt: None,
            cwd: None,
            profile: None,
            model: None,
            collector: Some(CollectorTask {
                launcher_config: PathBuf::from("/etc/geralt/launcher.json"),
                wall_timeout_secs: 3600,
                grace_ms: 5000,
            }),
            last_session_id: None,
            last_config_dir: None,
            last_run: None,
            last_status: None,
        }
    }

    fn collector_settings() -> CollectorSettings {
        CollectorSettings {
            node: PathBuf::from("/usr/bin/node"),
            geralt_repo: PathBuf::from("/srv/geralt"),
            export_dir: PathBuf::from("/var/lib/geralt/export"),
            runner_instance_id: "runner-1".into(),
        }
    }

    fn saved_version(t: &Tasks) -> u32 {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        save(&p, t).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        v["version"].as_u64().unwrap() as u32
    }

    #[test]
    fn collector_section_bumps_saved_version_to_2() {
        let t = Tasks {
            collector: Some(collector_settings()),
            ..Tasks::default()
        };
        assert_eq!(t.schema_version(), 2);
        assert_eq!(saved_version(&t), 2);
    }

    #[test]
    fn collector_task_alone_bumps_to_2() {
        let mut t = Tasks::default();
        t.tasks.insert("geralt-collect".into(), collector_entry());
        assert_eq!(t.collector, None);
        assert_eq!(t.schema_version(), 2);
        assert_eq!(saved_version(&t), 2);
    }

    #[test]
    fn removing_all_collector_content_saves_version_1() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        let mut t = Tasks {
            collector: Some(collector_settings()),
            ..Tasks::default()
        };
        t.tasks.insert("geralt-collect".into(), collector_entry());
        save(&p, &t).unwrap();

        let mut back = load(&p).unwrap();
        assert_eq!(back.version, 2);
        back.collector = None;
        back.tasks.remove("geralt-collect");
        assert_eq!(back.schema_version(), TASKS_VERSION);
        save(&p, &back).unwrap();
        assert_eq!(load(&p).unwrap().version, 1);
    }

    #[test]
    fn collector_content_round_trips_through_tasks_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        let mut t = Tasks {
            collector: Some(collector_settings()),
            ..Tasks::default()
        };
        t.tasks.insert("geralt-collect".into(), collector_entry());
        save(&p, &t).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"kind\": \"collector\""), "{raw}");
        let back = load(&p).unwrap();
        assert_eq!(back.collector, Some(collector_settings()));
        assert_eq!(
            back.tasks["geralt-collect"].collector,
            collector_entry().collector
        );
        assert_eq!(back.tasks["geralt-collect"].prompt, None);
    }

    #[test]
    fn load_rejects_version_3_and_accepts_2() {
        let dir = tempfile::tempdir().unwrap();
        let p = tasks_path(dir.path());
        std::fs::write(&p, br#"{"version":3,"tasks":{}}"#).unwrap();
        assert!(load(&p).unwrap_err().to_string().contains("newer"));
        std::fs::write(&p, br#"{"version":2,"tasks":{}}"#).unwrap();
        assert_eq!(load(&p).unwrap().version, 2);
    }

    #[test]
    fn collector_task_validates_and_has_no_effective_model() {
        let e = collector_entry();
        assert!(e.validate().is_ok());
        assert_eq!(e.effective_model(), None);
    }

    #[test]
    fn collector_task_requires_settings_struct() {
        let mut e = collector_entry();
        e.collector = None;
        assert!(e.validate().unwrap_err().to_string().contains("collector"));
    }

    #[test]
    fn collector_task_rejects_prompt_cwd_profile_model() {
        let mut e = collector_entry();
        e.prompt = Some("hi".into());
        assert!(e.validate().unwrap_err().to_string().contains("prompt"));

        let mut e = collector_entry();
        e.cwd = Some(PathBuf::from("/c"));
        assert!(e.validate().unwrap_err().to_string().contains("cwd"));

        let mut e = collector_entry();
        e.profile = Some("cortex".into());
        assert!(e.validate().unwrap_err().to_string().contains("profile"));

        let mut e = collector_entry();
        e.model = Some("haiku".into());
        assert!(e.validate().unwrap_err().to_string().contains("model"));
    }

    #[test]
    fn non_collector_rejects_collector_struct() {
        for mut e in [task_entry(), prime_entry()] {
            e.collector = collector_entry().collector;
            assert!(e.validate().unwrap_err().to_string().contains("collector"));
        }
    }

    fn prime_entry() -> TaskDef {
        let mut e = task_entry();
        e.kind = Kind::Prime;
        e.prompt = None;
        e.cwd = None;
        e.profile = None;
        e
    }

    #[test]
    fn grace_ms_bounds() {
        for (grace_ms, ok) in [
            (2499, false),
            (2500, true),
            (600_000, true),
            (600_001, false),
        ] {
            let mut e = collector_entry();
            e.collector.as_mut().unwrap().grace_ms = grace_ms;
            let r = e.validate();
            assert_eq!(r.is_ok(), ok, "grace_ms {grace_ms}: {r:?}");
            if !ok {
                assert!(r.unwrap_err().to_string().contains("grace_ms"));
            }
        }
    }

    #[test]
    fn wall_timeout_bounds() {
        for (secs, ok) in [(0, false), (1, true), (86_400, true), (86_401, false)] {
            let mut e = collector_entry();
            e.collector.as_mut().unwrap().wall_timeout_secs = secs;
            let r = e.validate();
            assert_eq!(r.is_ok(), ok, "wall_timeout_secs {secs}: {r:?}");
            if !ok {
                assert!(r.unwrap_err().to_string().contains("wall_timeout_secs"));
            }
        }
    }

    #[test]
    fn launcher_config_must_be_absolute() {
        let mut e = collector_entry();
        e.collector.as_mut().unwrap().launcher_config = PathBuf::from("launcher.json");
        assert!(e
            .validate()
            .unwrap_err()
            .to_string()
            .contains("launcher_config"));
    }

    #[test]
    fn is_collector_id_cases() {
        for ok in ["task-a", "a", "A_b.c:d-e", "0", &"a".repeat(200)] {
            assert!(is_collector_id(ok), "{ok:?} should be accepted");
        }
        for bad in [
            "",
            "-a",
            ".a",
            ":a",
            "_a",
            "a b",
            "a/b",
            "a\n",
            "é",
            "aé",
            &"a".repeat(201),
        ] {
            assert!(!is_collector_id(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn settings_validate() {
        assert!(collector_settings().validate().is_ok());

        let mut s = collector_settings();
        s.node = PathBuf::from("node");
        assert!(s.validate().unwrap_err().to_string().contains("node"));

        let mut s = collector_settings();
        s.geralt_repo = PathBuf::from("geralt");
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("geralt_repo"));

        let mut s = collector_settings();
        s.export_dir = PathBuf::from("./export");
        assert!(s.validate().unwrap_err().to_string().contains("export_dir"));

        for bad in ["", "-r", "a b", "é", &"a".repeat(201)] {
            let mut s = collector_settings();
            s.runner_instance_id = bad.into();
            assert!(
                s.validate()
                    .unwrap_err()
                    .to_string()
                    .contains("runner_instance_id"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_bad_runner_instance_id_message_reads_with_single_spaces() {
        let mut s = collector_settings();
        s.runner_instance_id = "-r".into();
        let msg = s.validate().unwrap_err().to_string();
        assert!(!msg.contains("  "), "stray spaces in {msg:?}");
        assert!(msg.contains("characters of letters, digits"), "{msg:?}");
    }
}
