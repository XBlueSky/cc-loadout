/// Shared test helpers for all profile sub-view test modules.
///
/// `ctx()` returns `(home_guard, data_guard, AppCtx)`.  The two `TempDir`
/// guards must be held by the caller for the entire test so the temporary
/// directories remain on disk.  The `AppCtx` owns only `PathBuf`s derived
/// from the guards, so no borrowing relationship is needed — but the
/// directories must not be dropped early.
///
/// `snap()` returns a minimal `Snapshot` suitable for test key-event
/// dispatch.
pub(crate) fn ctx() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    crate::tui::ctx::AppCtx,
) {
    use crate::account::paths;
    use crate::account::store::Store;
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let reg = home.path().join("installed_plugins.json");
    std::fs::write(&reg, r#"{"plugins":{}}"#).unwrap();
    let ctx = crate::tui::ctx::AppCtx {
        store: Store::new(data.path()),
        claude: paths::resolve(home.path(), None),
        home: home.path().to_path_buf(),
        data_root: data.path().to_path_buf(),
        cfg_path: home.path().join("profiles.json"),
        registry_path: reg,
        cwd: home.path().to_path_buf(),
    };
    (home, data, ctx)
}

pub(crate) fn snap() -> crate::tui::snapshot::Snapshot {
    crate::tui::snapshot::Snapshot {
        accounts: vec![],
        cwd: std::path::PathBuf::from("/tmp"),
        profiles_json_exists: false,
        matched: vec![],
        applied_count: 0,
        priming: vec![],
        schedule: Default::default(),
        global_enabled: Vec::new(),
        tasks: Vec::new(),
        schedule_drift: false,
    }
}
