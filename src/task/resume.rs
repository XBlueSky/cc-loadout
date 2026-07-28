use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

use crate::account::store::Store;
use crate::task::config::{self, TaskDef};

/// Resolve `(config_dir, session_id, cwd)` for the most recent run of a task.
pub(crate) fn resume_argv(def: &TaskDef) -> Result<(Option<PathBuf>, String, PathBuf)> {
    let sid = def
        .last_session_id
        .clone()
        .ok_or_else(|| anyhow!("task has never run — nothing to resume"))?;
    let cwd = def.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    Ok((def.last_config_dir.clone(), sid, cwd))
}

/// Exec `claude --resume <session>` in the task's cwd, under the recorded
/// CLAUDE_CONFIG_DIR (if the run was isolated). Replaces the current process.
pub fn resume(_store: &Store, data_root: &Path, _home: &Path, id: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let tasks = config::load(&config::tasks_path(data_root))?;
    let def = tasks
        .tasks
        .get(id)
        .ok_or_else(|| anyhow!("unknown task '{id}'"))?;
    let (cfg_dir, sid, cwd) = resume_argv(def)?;

    let mut cmd = std::process::Command::new("claude");
    cmd.arg("--resume").arg(&sid).current_dir(&cwd);
    if let Some(dir) = &cfg_dir {
        cmd.env("CLAUDE_CONFIG_DIR", dir);
    }
    let err = cmd.exec();
    Err(anyhow::Error::new(err).context("exec claude --resume"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::config::{Kind, TaskDef};
    use std::path::PathBuf;

    fn def_with_session() -> TaskDef {
        TaskDef {
            kind: Kind::Task,
            account: "work".into(),
            times: vec!["07:00".into()],
            prompt: Some("hi".into()),
            cwd: Some(PathBuf::from("/c")),
            profile: None,
            last_session_id: Some("dead-beef".into()),
            last_config_dir: Some(PathBuf::from("/data/accounts/work/run/cfg")),
            last_run: Some(1700),
            last_status: Some("ok".into()),
        }
    }

    #[test]
    fn resume_argv_returns_recorded_session_and_dir() {
        let (dir, sid, cwd) = resume_argv(&def_with_session()).unwrap();
        assert_eq!(sid, "dead-beef");
        assert_eq!(
            dir.as_deref(),
            Some(std::path::Path::new("/data/accounts/work/run/cfg"))
        );
        assert_eq!(cwd, std::path::PathBuf::from("/c"));
    }

    #[test]
    fn resume_argv_errors_when_never_run() {
        let mut d = def_with_session();
        d.last_session_id = None;
        assert!(resume_argv(&d)
            .unwrap_err()
            .to_string()
            .contains("never run"));
    }
}
