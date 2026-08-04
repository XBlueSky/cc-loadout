use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::account::cron::{self, render_tasks_block};
use crate::account::crontab;
use crate::account::store::Store;
use crate::task::config::{self, Kind, TaskDef, Tasks};

/// Everything needed to (re)generate the managed cron block.
pub struct CronContext {
    pub bin: PathBuf,
    pub cron_path: String,
    pub log: PathBuf,
}

/// Resolve the cc-loadout binary, the PATH to embed in cron lines (cc-loadout's
/// dir + claude's dir + standard dirs, deduped), and the prime log path.
pub fn cron_context(data_root: &Path) -> Result<CronContext> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let bin_dir = exe.parent().unwrap_or(Path::new("/")).to_path_buf();
    let mut dirs: Vec<String> = vec![bin_dir.display().to_string()];
    if let Some(claude_dir) =
        crate::util::which("claude").and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        let cd = claude_dir.display().to_string();
        if !dirs.contains(&cd) {
            dirs.push(cd);
        }
    }
    for d in ["/usr/local/bin", "/usr/bin", "/bin"] {
        if !dirs.iter().any(|x| x == d) {
            dirs.push(d.to_string());
        }
    }
    Ok(CronContext {
        bin: exe,
        cron_path: dirs.join(":"),
        log: data_root.join("prime.log"),
    })
}

/// The managed cron block that `tasks` should install (empty string = no block).
fn desired_block(tasks: &Tasks, data_root: &Path, home: &Path) -> Result<String> {
    let ctx = cron_context(data_root)?;
    Ok(if tasks.tasks.is_empty() {
        String::new()
    } else {
        render_tasks_block(tasks, &ctx.bin, &ctx.cron_path, home, &ctx.log)
    })
}

/// Whether the live crontab's managed region diverges from what `tasks.json` would
/// install. Lets the TUI warn when cron and the saved schedule fall out of sync (a
/// crontab wiped externally, or a past write that never landed) and offer a re-sync.
pub fn schedule_drift(crontab_bin: &Path, data_root: &Path, home: &Path) -> Result<bool> {
    let tasks = config::load(&config::tasks_path(data_root))?;
    let block = desired_block(&tasks, data_root, home)?;
    let live = crontab::read_with(crontab_bin)?;
    Ok(cron::managed_region(&live) != cron::managed_region(&block))
}

/// Regenerate the managed task block in the live crontab from `tasks`.
pub fn apply_cron(crontab_bin: &Path, tasks: &Tasks, data_root: &Path, home: &Path) -> Result<()> {
    let block = desired_block(tasks, data_root, home)?;
    let current = crontab::read_with(crontab_bin)?;
    let next = cron::splice(&current, &block, cron::TASK_BEGIN, cron::TASK_END);
    crontab::write_with(crontab_bin, &next)?;
    // Verify the write actually landed. A bare `crontab` that resolves to a sandbox
    // shim — or any silently-failing write — can return success while the live table
    // is unchanged, which is exactly how a schedule ends up saved-but-never-installed.
    let after = crontab::read_with(crontab_bin)?;
    if cron::managed_region(&after) != cron::managed_region(&next) {
        bail!(
            "crontab write did not take effect — the schedule was not installed into \
             cron. Check that `crontab` is the real system binary (not a sandbox shim) \
             and that your user crontab is writable."
        );
    }
    Ok(())
}

/// Create/replace task `id`; validate; persist tasks.json; regenerate cron.
pub fn add(
    crontab_bin: &Path,
    store: &Store,
    data_root: &Path,
    home: &Path,
    id: &str,
    def: TaskDef,
) -> Result<()> {
    crate::account::validate_alias(id)?;
    def.validate()?;
    if !store.load_state()?.accounts.contains_key(&def.account) {
        bail!("unknown account '{}'", def.account);
    }
    let path = config::tasks_path(data_root);
    let mut tasks = config::load(&path)?;
    if let Some(existing) = tasks.tasks.get(id) {
        if existing.kind != def.kind {
            bail!(
                "task id '{id}' already exists as a {:?} — choose a different id",
                existing.kind
            );
        }
    }
    tasks.tasks.insert(id.to_string(), def);
    // Install the crontab first: only persist tasks.json once cron actually holds the
    // schedule, so a failed install can never leave a phantom schedule the UI shows.
    apply_cron(crontab_bin, &tasks, data_root, home)?;
    config::save(&path, &tasks)
}

/// Remove task `id`; persist; regenerate cron.
pub fn remove(crontab_bin: &Path, data_root: &Path, home: &Path, id: &str) -> Result<()> {
    let path = config::tasks_path(data_root);
    let mut tasks = config::load(&path)?;
    tasks.tasks.remove(id);
    apply_cron(crontab_bin, &tasks, data_root, home)?;
    config::save(&path, &tasks)
}

/// Remove a PRIME task by alias (the `account schedule clear <alias>` path).
/// Refuses to delete a real (non-prime) task that happens to share the id.
pub fn remove_prime(crontab_bin: &Path, data_root: &Path, home: &Path, alias: &str) -> Result<()> {
    let path = config::tasks_path(data_root);
    let mut tasks = config::load(&path)?;
    if let Some(d) = tasks.tasks.get(alias) {
        if d.kind != crate::task::config::Kind::Prime {
            bail!("'{alias}' is a task, not a prime schedule — use `task rm {alias}`");
        }
    }
    tasks.tasks.remove(alias);
    apply_cron(crontab_bin, &tasks, data_root, home)?;
    config::save(&path, &tasks)
}

/// Print all scheduled tasks with their next fire times.
pub fn list(data_root: &Path, json: bool) -> Result<()> {
    use crate::account::timing::next_fire;
    use time::OffsetDateTime;

    let path = config::tasks_path(data_root);
    let tasks = config::load(&path)?;
    let now = OffsetDateTime::now_utc();

    if json {
        println!("{}", serde_json::to_string_pretty(&tasks)?);
        return Ok(());
    }

    if tasks.tasks.is_empty() {
        println!("No scheduled tasks.");
        return Ok(());
    }

    for (id, def) in &tasks.tasks {
        let next = next_fire(&def.times, now)
            .map(|t| format!("{}", t))
            .unwrap_or_else(|| "—".to_string());
        // Show the model the run will actually use — including the haiku a prime
        // is forced onto — so `list` never leaves the reader guessing which tier
        // a scheduled run bills against.
        println!(
            "{id}  account={acc}  times={times}  model={model}  next={next}",
            acc = def.account,
            times = def.times.join(","),
            model = def.effective_model().unwrap_or("(cli default)"),
        );
    }
    Ok(())
}

/// Prime-kind tasks as an `alias -> times` map (the schedule the TUI / `account
/// schedule` view edits). Prime task ids ARE the account alias.
pub fn load_prime_times(data_root: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    let tasks = config::load(&config::tasks_path(data_root))?;
    Ok(tasks
        .tasks
        .into_iter()
        .filter(|(_, d)| d.kind == Kind::Prime)
        .map(|(id, d)| (id, d.times))
        .collect())
}

/// Trusted full-replace of the prime schedule: drop ALL existing prime-kind tasks,
/// insert one `kind: Prime` task per `(alias, times)` in `map` (keyed by alias),
/// preserving every non-prime task, then regenerate cron. Empty time lists are
/// skipped. No account validation (the caller's working copy holds real aliases).
pub fn write_prime_schedule(
    crontab_bin: &Path,
    data_root: &Path,
    home: &Path,
    map: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let path = config::tasks_path(data_root);
    let mut tasks = config::load(&path)?;
    tasks.tasks.retain(|_, d| d.kind != Kind::Prime);
    for (alias, times) in map {
        if times.is_empty() {
            continue;
        }
        tasks.tasks.insert(
            alias.clone(),
            TaskDef {
                kind: Kind::Prime,
                account: alias.clone(),
                times: times.clone(),
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
    }
    apply_cron(crontab_bin, &tasks, data_root, home)?;
    config::save(&path, &tasks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::store::{AccountMeta, Store};
    use crate::task::config::{Kind, TaskDef};
    use std::os::unix::fs::PermissionsExt;

    fn fake_crontab(bin_dir: &std::path::Path, store_file: &std::path::Path) -> std::path::PathBuf {
        let script = format!(
            "#!/bin/sh\nSTORE='{s}'\nif [ \"$1\" = '-l' ]; then [ -f \"$STORE\" ] && cat \"$STORE\" || exit 1; elif [ \"$1\" = '-' ]; then cat > \"$STORE\"; else exit 2; fi\n",
            s = store_file.display()
        );
        let p = bin_dir.join("crontab");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A `crontab` that silently swallows writes — `-` consumes stdin but persists
    /// nothing and `-l` is always empty. Models the real failure this fix targets:
    /// a bare `crontab` resolving to a sandbox shim (or an otherwise no-op write)
    /// that returns success while the live table never changes.
    fn fake_crontab_noop(bin_dir: &std::path::Path) -> std::path::PathBuf {
        let script =
            "#!/bin/sh\nif [ \"$1\" = '-l' ]; then exit 0; elif [ \"$1\" = '-' ]; then cat >/dev/null; else exit 2; fi\n";
        let p = bin_dir.join("crontab");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn apply_cron_errors_when_write_does_not_persist() {
        let bin = tempfile::tempdir().unwrap();
        let crontab = fake_crontab_noop(bin.path());
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        map.insert("work".to_string(), vec!["08:00".to_string()]);
        let err = write_prime_schedule(&crontab, data.path(), home.path(), &map).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("did not take effect") || msg.contains("not installed"),
            "a swallowed crontab write must surface as an error; got: {err}"
        );
    }

    #[test]
    fn failed_crontab_write_does_not_persist_tasks_json() {
        // apply-cron-before-save: when the crontab cannot be installed, the schedule
        // must NOT be recorded in tasks.json — otherwise the UI shows an "active"
        // schedule that cron never received (the exact bug this fix targets).
        let bin = tempfile::tempdir().unwrap();
        let crontab = fake_crontab_noop(bin.path());
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        map.insert("work".to_string(), vec!["08:00".to_string()]);
        let _ = write_prime_schedule(&crontab, data.path(), home.path(), &map);
        let path = config::tasks_path(data.path());
        let persisted = config::load(&path).unwrap();
        assert!(
            persisted.tasks.is_empty(),
            "no schedule should be persisted when the crontab install fails"
        );
    }

    #[test]
    fn schedule_drift_false_after_write_true_after_external_wipe() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut map = BTreeMap::new();
        map.insert("work".to_string(), vec!["08:00".to_string()]);
        write_prime_schedule(&crontab, data.path(), home.path(), &map).unwrap();
        assert!(
            !schedule_drift(&crontab, data.path(), home.path()).unwrap(),
            "a freshly written schedule is in sync with the crontab"
        );

        // Simulate the crontab being cleared out from under us (the observed
        // empty-crontab-with-live-tasks.json state).
        crate::account::crontab::write_with(&crontab, "").unwrap();
        assert!(
            schedule_drift(&crontab, data.path(), home.path()).unwrap(),
            "a wiped crontab with a non-empty tasks.json is drift"
        );
    }

    #[test]
    fn schedule_drift_false_when_both_empty() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        // No tasks.json written, empty crontab → no drift (nothing to install).
        assert!(!schedule_drift(&crontab, data.path(), home.path()).unwrap());
    }

    #[test]
    fn add_writes_task_block_preserving_foreign_jobs() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        std::fs::write(&storef, "0 0 * * * backup.sh\n").unwrap();
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        store.save_state(&st).unwrap();

        add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "weekly",
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(std::path::PathBuf::from("/c")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap();

        let tab = std::fs::read_to_string(&storef).unwrap();
        assert!(tab.contains("0 0 * * * backup.sh"), "foreign job preserved");
        assert!(tab.contains("task run weekly --quiet"));
    }

    #[test]
    fn write_prime_then_load_roundtrips_and_preserves_task_entry() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            crate::account::store::AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        store.save_state(&st).unwrap();

        // Pre-insert a non-prime Task entry that must survive.
        add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "weekly",
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(std::path::PathBuf::from("/c")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap();

        // Write prime schedule: alias "work" → "06:00".
        let mut map = BTreeMap::new();
        map.insert("work".into(), vec!["06:00".into()]);
        write_prime_schedule(&crontab, data.path(), home.path(), &map).unwrap();

        // load_prime_times returns only the prime entry.
        let back = load_prime_times(data.path()).unwrap();
        assert_eq!(back.get("work").unwrap(), &vec!["06:00".to_string()]);
        assert!(
            !back.contains_key("weekly"),
            "non-prime must not appear in prime map"
        );

        // The non-prime 'weekly' task must still exist on disk.
        let all_tasks = config::load(&config::tasks_path(data.path())).unwrap();
        assert!(
            all_tasks.tasks.contains_key("weekly"),
            "non-prime task must be preserved"
        );
        assert_eq!(all_tasks.tasks["weekly"].kind, Kind::Task);
    }

    #[test]
    fn load_prime_times_returns_only_prime_entries() {
        let data = tempfile::tempdir().unwrap();

        // Insert one Prime and one Task directly into tasks.json.
        let mut tasks = Tasks::default();
        tasks.tasks.insert(
            "work".into(),
            TaskDef {
                kind: Kind::Prime,
                account: "work".into(),
                times: vec!["08:00".into()],
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        tasks.tasks.insert(
            "weekly".into(),
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(std::path::PathBuf::from("/c")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        );
        config::save(&config::tasks_path(data.path()), &tasks).unwrap();

        let prime_map = load_prime_times(data.path()).unwrap();
        assert_eq!(prime_map.len(), 1, "only the Prime entry should appear");
        assert_eq!(prime_map["work"], vec!["08:00".to_string()]);
        assert!(!prime_map.contains_key("weekly"));
    }

    #[test]
    fn remove_empties_block() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        store.save_state(&st).unwrap();
        add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "weekly",
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hi".into()),
                cwd: Some(std::path::PathBuf::from("/c")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap();
        remove(&crontab, data.path(), home.path(), "weekly").unwrap();
        let tab = std::fs::read_to_string(&storef).unwrap();
        assert!(!tab.contains(crate::account::cron::TASK_BEGIN));
    }

    #[test]
    fn add_rejects_invalid_id_charset() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        // No need to populate accounts: validate_alias fires first.
        let err = add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "bad;id",
            TaskDef {
                kind: Kind::Prime,
                account: "any".into(),
                times: vec!["07:00".into()],
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bad;id"),
            "error should mention the invalid id; got: {msg}"
        );
    }

    #[test]
    fn add_rejects_kind_mismatch_on_existing_id() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        store.save_state(&st).unwrap();

        // Insert a real Task under id "x".
        add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "x",
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hello".into()),
                cwd: Some(std::path::PathBuf::from("/tmp")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap();

        // Attempt to overwrite it with a Prime of the same id → must fail.
        let err = add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "x",
            TaskDef {
                kind: Kind::Prime,
                account: "work".into(),
                times: vec!["08:00".into()],
                prompt: None,
                cwd: None,
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("already exists"),
            "error should mention 'already exists'; got: {msg}"
        );
    }

    #[test]
    fn remove_prime_refuses_real_task() {
        let bin = tempfile::tempdir().unwrap();
        let storef = bin.path().join("tab");
        let crontab = fake_crontab(bin.path(), &storef);
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let store = Store::new(data.path());
        let mut st = store.load_state().unwrap();
        st.accounts.insert(
            "work".into(),
            AccountMeta {
                email: "a@b".into(),
                ..Default::default()
            },
        );
        store.save_state(&st).unwrap();

        // Insert a real Task under id "x".
        add(
            &crontab,
            &store,
            data.path(),
            home.path(),
            "x",
            TaskDef {
                kind: Kind::Task,
                account: "work".into(),
                times: vec!["07:00".into()],
                prompt: Some("hello".into()),
                cwd: Some(std::path::PathBuf::from("/tmp")),
                profile: None,
                model: None,
                last_session_id: None,
                last_config_dir: None,
                last_run: None,
                last_status: None,
            },
        )
        .unwrap();

        // remove_prime on a Task id must be refused.
        let err = remove_prime(&crontab, data.path(), home.path(), "x").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("task, not a prime"),
            "error should say 'task, not a prime'; got: {msg}"
        );
    }
}
