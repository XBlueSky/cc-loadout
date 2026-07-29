use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn write_login(home: &Path, email: &str) {
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude").join(".credentials.json"),
        format!(r#"{{"claudeAiOauth":{{"accessToken":"tok-{email}"}}}}"#),
    )
    .unwrap();
    std::fs::write(
        home.join(".claude.json"),
        format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}}}}"#),
    )
    .unwrap();
}

/// The ONLY constructor for the cc-loadout binary under test. It pins HOME and
/// XDG_DATA_HOME to throwaway temp dirs and removes CLAUDE_CONFIG_DIR, so no test
/// (and no `claude` subprocess it might spawn) can ever read or write the
/// developer's real ~/.claude config. It ALSO puts a file-backed fake `crontab`
/// first on PATH, so no test can splice the developer's real user crontab — even one
/// that forgets to patch PATH itself (the gap that once wiped a live prime schedule).
/// Do NOT call `Command::cargo_bin("cc-loadout")` directly anywhere in this file —
/// always go through `cmd()` and chain extra `.env(...)` / `.current_dir(...)` as
/// needed. A test that must inspect the table overrides PATH with its own
/// `fake_crontab_path(dir)` and reads that dir's `tab`.
fn cmd(home: &Path, data: &Path) -> Command {
    let mut c = Command::cargo_bin("cc-loadout").unwrap();
    // Default crontab isolation. The fake bin lives under the caller-owned `data`
    // temp dir (test lifetime) so it outlives the spawned command; a tempdir created
    // here would be dropped on return, leaving PATH pointing at a deleted directory.
    let fakebin = data.join(".fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let fake_path = fake_crontab_path(&fakebin);
    c.env("HOME", home)
        .env("XDG_DATA_HOME", data)
        .env("PATH", fake_path)
        .env_remove("CLAUDE_CONFIG_DIR");
    c
}

/// Install a file-backed fake `crontab` in `bin_dir` and return the PATH string to
/// hand the command (fake dir first). Unlike a `>/dev/null` stub, this persists the
/// table to `bin_dir/tab`, so the binary's write-then-read-back verification (which
/// guards against a `crontab` that silently swallows writes) round-trips instead of
/// failing — while still never touching the developer's real user crontab.
fn fake_crontab_path(bin_dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let store = bin_dir.join("tab");
    let script = format!(
        "#!/bin/sh\nSTORE='{s}'\nif [ \"$1\" = '-l' ]; then [ -f \"$STORE\" ] && cat \"$STORE\" || exit 1; elif [ \"$1\" = '-' ]; then cat > \"$STORE\"; else exit 2; fi\n",
        s = store.display()
    );
    let bin = bin_dir.join("crontab");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn full_account_cycle() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    write_login(home, "personal@x");
    cmd(home, data)
        .args(["account", "add", "personal"])
        .assert()
        .success();

    cmd(home, data)
        .args(["account", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work@x").and(predicate::str::contains("personal@x")));

    cmd(home, data)
        .args(["account", "use", "work"])
        .assert()
        .success();

    let cfg = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    assert!(cfg.contains("work@x"));
    let creds = std::fs::read_to_string(home.join(".claude").join(".credentials.json")).unwrap();
    assert!(creds.contains("tok-work@x"));

    cmd(home, data)
        .args(["account", "current"])
        .assert()
        .success()
        .stdout(predicate::str::contains("work"));

    cmd(home, data)
        .args(["account", "rm", "personal"])
        .assert()
        .success();
    cmd(home, data)
        .args(["account", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("personal@x").not());
}

fn write_profiles(home: &Path) -> std::path::PathBuf {
    let p = home.join("profiles.json");
    std::fs::write(
        &p,
        r#"{
            "scan_roots": [],
            "universal": ["u@m"],
            "profiles": {
                "frontend": {"plugins": ["fe@m"], "detect": {"marker_globs": ["*.vue"]}},
                "backend": {"plugins": ["be@m"], "detect": {"marker_files": ["INFO"]}}
            }
        }"#,
    )
    .unwrap();
    p
}

#[test]
fn profile_detect_and_apply_cycle() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let profiles = write_profiles(hdir.path());
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    let run = |args: &[&str]| {
        let mut c = cmd(hdir.path(), ddir.path());
        c.env("CC_LOADOUT_PROFILES", &profiles).args(args);
        c
    };

    run(&["profile", "detect", repo.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("frontend"));

    run(&["profile", "apply", repo.path().to_str().unwrap()])
        .assert()
        .success();

    let settings: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repo.path().join(".claude").join("settings.local.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["enabledPlugins"]["fe@m"], serde_json::json!(true));
    assert_eq!(settings["enabledPlugins"]["u@m"], serde_json::json!(true));
    assert_eq!(settings["enabledPlugins"]["be@m"], serde_json::json!(false));

    run(&["profile", "status", repo.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("fe@m"));
}

#[test]
fn account_use_does_not_touch_profile_settings() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    // Two accounts.
    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();
    write_login(home, "personal@x");
    cmd(home, data)
        .args(["account", "add", "personal"])
        .assert()
        .success();

    // A repo with an existing per-repo settings.local.json (profile slot state).
    let settings = repo.path().join(".claude").join("settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"enabledPlugins":{"ondemand@m":true},"theme":"dark"}"#,
    )
    .unwrap();
    let before = std::fs::read(&settings).unwrap();

    // Switch the ACCOUNT slot.
    cmd(home, data)
        .args(["account", "use", "work"])
        .assert()
        .success();

    // The PROFILE slot's file must be byte-identical.
    let after = std::fs::read(&settings).unwrap();
    assert_eq!(
        before, after,
        "account use must not mutate profile settings.local.json"
    );
}

#[test]
fn profile_apply_does_not_touch_account_credentials() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();
    let profiles = write_profiles(home);

    // Account slot state: a live login.
    write_login(home, "work@x");
    let creds_path = home.join(".claude").join(".credentials.json");
    let config_path = home.join(".claude.json");
    let creds_before = std::fs::read(&creds_path).unwrap();
    let config_before = std::fs::read(&config_path).unwrap();

    // A repo that matches a profile.
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    // Apply the PROFILE slot.
    cmd(home, data)
        .env("CC_LOADOUT_PROFILES", &profiles)
        .args(["profile", "apply", repo.path().to_str().unwrap()])
        .assert()
        .success();

    // The ACCOUNT slot's files must be byte-identical.
    assert_eq!(
        creds_before,
        std::fs::read(&creds_path).unwrap(),
        "profile apply must not mutate ~/.claude/.credentials.json"
    );
    assert_eq!(
        config_before,
        std::fs::read(&config_path).unwrap(),
        "profile apply must not mutate ~/.claude.json (incl. oauthAccount)"
    );
}

#[test]
fn profile_force_writes_override() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let profiles = write_profiles(hdir.path());

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .current_dir(repo.path())
        .args(["profile", "force", "backend"])
        .assert()
        .success();

    let body = std::fs::read_to_string(repo.path().join(".claude").join("profile")).unwrap();
    assert_eq!(body, "backend\n");
}

#[test]
fn profile_inventory_json() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();

    let app = roots.path().join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{"dependencies":{"react":"18"}}"#,
    )
    .unwrap();

    let profiles = hdir.path().join("profiles.json");
    std::fs::write(
        &profiles,
        format!(
            r#"{{"scan_roots":["{}"],"universal":[],"profiles":{{}}}}"#,
            roots.path().display()
        ),
    )
    .unwrap();

    std::fs::create_dir_all(hdir.path().join(".claude").join("plugins")).unwrap();
    std::fs::write(
        hdir.path()
            .join(".claude")
            .join("plugins")
            .join("installed_plugins.json"),
        r#"{"plugins":{"serena@official":[{"scope":"user"}]}}"#,
    )
    .unwrap();

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .args(["profile", "inventory", "--json"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("serena@official")
                .and(predicate::str::contains("suggested_profiles"))
                .and(predicate::str::contains("frontend"))
                .and(predicate::str::contains("\"schema_version\"")),
        );
}

#[test]
fn profile_inventory_works_without_existing_profiles_file() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();
    let app = roots.path().join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    std::fs::write(app.join("Cargo.toml"), "[package]").unwrap();

    // CC_LOADOUT_PROFILES points at a path that does NOT exist
    let missing = hdir.path().join("nope").join("profiles.json");

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &missing)
        .args([
            "profile",
            "inventory",
            "--json",
            "--root",
            roots.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"rust\""));
}

#[test]
fn status_shows_both_slots() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();
    let profiles = write_profiles(home);

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    cmd(home, data)
        .env("CC_LOADOUT_PROFILES", &profiles)
        .current_dir(repo.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Account:")
                .and(predicate::str::contains("work@x"))
                .and(predicate::str::contains("Profile (cwd:"))
                .and(predicate::str::contains("frontend")),
        );
}

#[test]
fn account_list_json_emits_versioned_envelope() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    let out = cmd(home, data)
        .args(["account", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert_eq!(v["accounts"][0]["alias"], serde_json::json!("work"));
    assert_eq!(v["accounts"][0]["email"], serde_json::json!("work@x"));
    assert_eq!(v["accounts"][0]["active"], serde_json::json!(true));
}

#[test]
fn account_current_json_reports_active_alias() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    let out = cmd(home, data)
        .args(["account", "current", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert_eq!(v["active"], serde_json::json!("work"));
}

#[test]
fn status_json_has_two_sections_and_version() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();
    let profiles = write_profiles(home);

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    let out = cmd(home, data)
        .env("CC_LOADOUT_PROFILES", &profiles)
        .current_dir(repo.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert_eq!(
        v["account"]["accounts"][0]["alias"],
        serde_json::json!("work")
    );
    assert!(v["profile"]["cwd"].is_string());
    assert_eq!(v["profile"]["matched"], serde_json::json!(["frontend"]));
}

#[test]
fn profile_detect_json_lists_matched_and_plugins() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let profiles = write_profiles(hdir.path());
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    let out = cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .args(["profile", "detect", repo.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert_eq!(v["repos"][0]["matched"], serde_json::json!(["frontend"]));
    assert!(v["repos"][0]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "fe@m"));
}

#[test]
fn profile_apply_then_status_json() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let profiles = write_profiles(hdir.path());
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();
    let run = |args: &[&str]| {
        let mut c = cmd(hdir.path(), ddir.path());
        c.env("CC_LOADOUT_PROFILES", &profiles).args(args);
        c
    };

    let out = run(&["profile", "apply", repo.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["repos"][0]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["plugin"] == "fe@m" && c["to"] == serde_json::json!(true)));

    let out2 = run(&["profile", "status", repo.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let v2: serde_json::Value = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(v2["schema_version"], serde_json::json!(1));
    assert!(v2["repos"][0]["applied"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "fe@m"));
}

#[test]
fn profile_detect_json_reports_signals() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let profiles = write_profiles(hdir.path());
    std::fs::write(repo.path().join("App.vue"), "x").unwrap();

    let out = cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .args(["profile", "detect", repo.path().to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["repos"][0]["signals"][0]["profile"],
        serde_json::json!("frontend")
    );
    assert_eq!(
        v["repos"][0]["signals"][0]["rule"],
        serde_json::json!("marker_glob")
    );
    assert_eq!(
        v["repos"][0]["signals"][0]["value"],
        serde_json::json!("*.vue")
    );
}

#[test]
fn schedule_list_json_has_next_fire_and_last_primed() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    let bin = tempfile::tempdir().unwrap();
    let patched_path = fake_crontab_path(bin.path());
    cmd(home, data)
        .env("PATH", &patched_path)
        .args(["account", "schedule", "set", "work", "06:00", "23:59"])
        .assert()
        .success();

    let out = cmd(home, data)
        .env("PATH", &patched_path)
        .args(["account", "schedule", "list", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], serde_json::json!(1));
    assert_eq!(v["schedule"]["work"], serde_json::json!(["06:00", "23:59"]));
    assert!(v["next_fire"]["work"].is_string());
    assert_eq!(v["last_primed"]["work"], serde_json::json!(null));
}

#[test]
fn cmd_isolates_crontab_from_the_real_system_by_default() {
    // Regression for the wipe: a crontab-WRITING subcommand run through cmd() with NO
    // explicit PATH patch must land on cmd()'s built-in fake crontab, never the
    // developer's real user crontab. Before this guard, an unpatched write test spliced
    // the live prime schedule out of the real crontab.
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();
    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    // Deliberately NO .env("PATH", ...) — this is the footgun path. It must still be
    // isolated by cmd() itself.
    cmd(home, data)
        .args(["account", "schedule", "set", "work", "06:00"])
        .assert()
        .success();

    let tab = std::fs::read_to_string(data.join(".fakebin").join("tab"))
        .expect("cmd() must install a fake crontab store under the data dir");
    assert!(
        tab.contains("task run work --quiet"),
        "the schedule write must land on the fake crontab, not the real one; got: {tab}"
    );
}

#[test]
fn status_json_has_priming_section() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();
    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    let bin = tempfile::tempdir().unwrap();
    let patched_path = fake_crontab_path(bin.path());
    cmd(home, data)
        .env("PATH", &patched_path)
        .args(["account", "schedule", "set", "work", "06:00"])
        .assert()
        .success();

    let out = cmd(home, data)
        .env("PATH", &patched_path)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["priming"]["work"]["next_fire"].is_string());
    assert_eq!(v["priming"]["work"]["last_primed"], serde_json::json!(null));
    assert!(v["account"]["accounts"].is_array());
    assert!(v["profile"]["cwd"].is_string());
}

#[test]
fn use_launch_without_claude_on_path_warns_but_swaps() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();
    write_login(home, "personal@x");
    cmd(home, data)
        .args(["account", "add", "personal"])
        .assert()
        .success();

    // Force an empty PATH so `claude` is unresolvable; the swap must still succeed.
    cmd(home, data)
        .env("PATH", "")
        .args(["account", "use", "work", "--launch"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not found on PATH"));

    // Credentials actually swapped to work.
    let creds = std::fs::read_to_string(home.join(".claude").join(".credentials.json")).unwrap();
    assert!(creds.contains("tok-work@x"));
}

#[test]
fn bare_account_non_tty_does_not_launch_tui() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    // Non-TTY: prints the headless hint and exits 0.
    cmd(hdir.path(), ddir.path())
        .args(["account"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is interactive and needs a terminal",
        ));
}

#[test]
fn bare_schedule_non_tty_uses_fallback() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    // Non-TTY: prints the headless hint and exits 0.
    cmd(hdir.path(), ddir.path())
        .args(["account", "schedule"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is interactive and needs a terminal",
        ));
}

#[test]
fn bare_task_non_tty_uses_fallback() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    // Non-TTY: `cc-loadout task` with no subcommand prints the headless hint
    // (does not launch the Tasks-tab TUI) and exits 0.
    cmd(hdir.path(), ddir.path())
        .args(["task"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is interactive and needs a terminal",
        ));
}

#[test]
fn profile_init_non_tty_uses_fallback_not_tui() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let roots = tempfile::tempdir().unwrap();
    let profiles = hdir.path().join("profiles.json");
    // Non-TTY: prints the headless hint and exits 0 (deterministic; no hang).
    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .args(["profile", "init", "--root", roots.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "is interactive and needs a terminal",
        ));
}

#[test]
fn bare_command_non_tty_prints_status_snapshot() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    // No args + captured (non-tty) stdout -> must degrade to the status snapshot,
    // never block on a TUI.
    cmd(home, data)
        .assert()
        .success()
        .stdout(predicate::str::contains("Account:").and(predicate::str::contains("work@x")));
}

#[test]
fn profile_init_noninteractive_writes_and_applies_global() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();

    // installed-plugin registry
    let pdir = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(
        pdir.join("installed_plugins.json"),
        r#"{"plugins":{"serena@official":[{"scope":"user"}],"rust-analyzer@community":[{"scope":"user"}]}}"#,
    )
    .unwrap();

    // scan root with a rust repo
    let root = home.join("repos");
    let repo = root.join("app");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(repo.join("Cargo.toml"), "[package]").unwrap();

    // assignment file
    let assign = home.join("assign.json");
    std::fs::write(
        &assign,
        r#"{"universal":["serena@official"],"profiles":{"rust":["rust-analyzer@community"]}}"#,
    )
    .unwrap();

    cmd(home, ddir.path())
        .args([
            "profile",
            "init",
            "--root",
            root.to_str().unwrap(),
            "--assign",
            assign.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("rust-analyzer@community")
                .and(predicate::str::contains("apply --all")),
        );

    // profiles.json written under ~/.claude/profiles/
    let cfg = std::fs::read_to_string(home.join(".claude").join("profiles").join("profiles.json"))
        .unwrap();
    assert!(cfg.contains("rust") && cfg.contains("Cargo.toml"));
    // global settings.json applied (universal on, profile-specific off)
    let s = std::fs::read_to_string(home.join(".claude").join("settings.json")).unwrap();
    assert!(
        s.contains("\"serena@official\": true"),
        "universal plugin enabled globally: {s}"
    );
    assert!(
        s.contains("\"rust-analyzer@community\": false"),
        "profile-specific plugin disabled globally: {s}"
    );

    // --assign without --root is an error
    cmd(home, ddir.path())
        .args(["profile", "init", "--assign", assign.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--root"));
}

#[test]
fn on_demand_acquire_then_release_round_trips_enabled_plugins() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let profiles = hdir.path().join("profiles.json");
    std::fs::write(
        &profiles,
        r#"{"scan_roots":[],"universal":[],"profiles":{},"on_demand":["pixijs@x"]}"#,
    )
    .unwrap();

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .env("CC_LOADOUT_SESSION_ID", "sess-cli-1")
        .current_dir(repo.path())
        .args(["profile", "on-demand", "acquire", "pixijs@x"])
        .assert()
        .success();

    let settings =
        std::fs::read_to_string(repo.path().join(".claude").join("settings.local.json")).unwrap();
    // write_enabled (Task 3) serializes with serde_json::to_vec_pretty, so the
    // colon has a trailing space — not the compact `"pixijs@x":true` form.
    assert!(settings.contains("\"pixijs@x\": true"));

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .env("CC_LOADOUT_SESSION_ID", "sess-cli-1")
        .current_dir(repo.path())
        .args(["profile", "on-demand", "release", "pixijs@x"])
        .assert()
        .success();

    let settings =
        std::fs::read_to_string(repo.path().join(".claude").join("settings.local.json")).unwrap();
    assert!(!settings.contains("\"pixijs@x\": true"));
}

#[test]
fn on_demand_acquire_rejects_key_not_in_on_demand_list() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let profiles = hdir.path().join("profiles.json");
    std::fs::write(
        &profiles,
        r#"{"scan_roots":[],"universal":[],"profiles":{},"on_demand":[]}"#,
    )
    .unwrap();

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .env("CC_LOADOUT_SESSION_ID", "sess-cli-1")
        .current_dir(repo.path())
        .args(["profile", "on-demand", "acquire", "pixijs@x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not in on_demand"));
}

#[test]
fn task_add_and_list() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let cwd_dir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let data = ddir.path();

    // Set up an account snapshot so task add can validate the account exists.
    write_login(home, "work@x");
    cmd(home, data)
        .args(["account", "add", "work"])
        .assert()
        .success();

    // Provide a fake crontab binary so task add doesn't fail in the test env.
    let bin = tempfile::tempdir().unwrap();
    let patched_path = fake_crontab_path(bin.path());

    // task add creates the task.
    cmd(home, data)
        .env("PATH", &patched_path)
        .args([
            "task",
            "add",
            "weekly",
            "--account",
            "work",
            "--at",
            "07:00",
            "--prompt",
            "/cortex:weekly",
            "--cwd",
            cwd_dir.path().to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("weekly"));

    // task list --json reports the task with its prompt.
    cmd(home, data)
        .args(["task", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("weekly").and(predicate::str::contains("/cortex:weekly")));
}

#[test]
fn hook_session_start_exports_the_session_id() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let env_file = ddir.path().join("env");

    cmd(home, ddir.path())
        .args(["hook", "session-start"])
        .env("CLAUDE_ENV_FILE", &env_file)
        .write_stdin(r#"{"session_id":"sess-abc"}"#)
        .assert()
        .success();

    let body = std::fs::read_to_string(&env_file).unwrap();
    assert!(
        body.contains("export CC_LOADOUT_SESSION_ID=sess-abc"),
        "env file was: {body}"
    );
}

#[test]
fn hook_session_start_promotes_managed_plugins_to_user_scope() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();

    std::fs::create_dir_all(home.join(".claude").join("profiles")).unwrap();
    std::fs::write(
        home.join(".claude").join("profiles").join("profiles.json"),
        r#"{"scan_roots":[],"universal":["u@m"],"profiles":{},"on_demand":[]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.join(".claude").join("plugins")).unwrap();
    let reg = home
        .join(".claude")
        .join("plugins")
        .join("installed_plugins.json");
    std::fs::write(
        &reg,
        r#"{"version":2,"plugins":{"u@m":[{"scope":"local","projectPath":"/p","lastUpdated":"1"}]}}"#,
    )
    .unwrap();

    cmd(home, ddir.path())
        .args(["hook", "session-start"])
        .write_stdin(r#"{"session_id":"s"}"#)
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&reg).unwrap()).unwrap();
    assert_eq!(v["plugins"]["u@m"][0]["scope"], "user");
}

#[test]
fn hook_session_start_migrates_legacy_settings_hooks() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude").join("settings.json");
    std::fs::write(
        &settings,
        r#"{"hooks":{"SessionStart":[
            {"hooks":[{"type":"command","command":"bash \"/old/lib/session-start-hook.sh\""}]}
        ]}}"#,
    )
    .unwrap();

    cmd(home, ddir.path())
        .args(["hook", "session-start"])
        .write_stdin(r#"{"session_id":"s"}"#)
        .assert()
        .success();

    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert!(
        v["hooks"].get("SessionStart").is_none(),
        "the retired entry should be gone: {v}"
    );
}

#[test]
fn hook_session_start_survives_garbage_stdin() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    cmd(hdir.path(), ddir.path())
        .args(["hook", "session-start"])
        .write_stdin("not json at all")
        .assert()
        .success();
}

#[test]
fn hook_session_end_is_a_noop_without_a_session_id() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    cmd(hdir.path(), ddir.path())
        .args(["hook", "session-end"])
        .write_stdin(r#"{"cwd":"/tmp"}"#)
        .assert()
        .success();
}

#[test]
fn hook_session_start_rejects_a_hostile_session_id_in_the_env_file() {
    // `$CLAUDE_ENV_FILE` is sourced by a shell later in the session. A session
    // id is JSON-string input from Claude Code, which can legally contain a
    // newline (splits into a second command line) or `$(...)`/backticks
    // (command substitution). Regression test for the fix: the hook must
    // never write such a value into the file, even though it must still
    // exit successfully (a hook must never block a session).
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let env_file = ddir.path().join("env");

    let hostile = "abc\n$(touch /tmp/pwned)";
    let stdin = serde_json::json!({ "session_id": hostile }).to_string();

    cmd(home, ddir.path())
        .args(["hook", "session-start"])
        .env("CLAUDE_ENV_FILE", &env_file)
        .write_stdin(stdin)
        .assert()
        .success();

    if env_file.exists() {
        let body = std::fs::read_to_string(&env_file).unwrap();
        assert!(
            !body
                .lines()
                .any(|l| l.starts_with("export CC_LOADOUT_SESSION_ID=abc")),
            "a hostile session id must never be written into the sourced env file: {body}"
        );
    }
}

#[test]
fn hook_session_end_releases_the_sessions_on_demand_holds() {
    // Positive-path coverage for session_end's actual wiring to
    // `profile::on_demand::release_all` — the earlier noop test only covers
    // the early-return guard. Drives `acquire` through the real CLI (not
    // hand-written state) so the round trip through `hook session-end`
    // exercises the same on-demand state and settings.local.json the CLI
    // itself would produce.
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();

    let profiles = hdir.path().join("profiles.json");
    std::fs::write(
        &profiles,
        r#"{"scan_roots":[],"universal":[],"profiles":{},"on_demand":["pixijs@x"]}"#,
    )
    .unwrap();

    // A prior value the release must restore.
    let settings = repo.path().join(".claude").join("settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"enabledPlugins":{"pixijs@x":false}}"#).unwrap();

    cmd(hdir.path(), ddir.path())
        .env("CC_LOADOUT_PROFILES", &profiles)
        .env("CC_LOADOUT_SESSION_ID", "sess-hookend-1")
        .current_dir(repo.path())
        .args(["profile", "on-demand", "acquire", "pixijs@x"])
        .assert()
        .success();

    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(after["enabledPlugins"]["pixijs@x"], serde_json::json!(true));

    let stdin = format!(
        r#"{{"session_id":"sess-hookend-1","cwd":"{}"}}"#,
        repo.path().display()
    );
    cmd(hdir.path(), ddir.path())
        .args(["hook", "session-end"])
        .write_stdin(stdin)
        .assert()
        .success();

    let state_path = repo
        .path()
        .join(".claude")
        .join(".cc-loadout")
        .join("on-demand.json");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert!(
        state.get("pixijs@x").is_none(),
        "the released key must be dropped from on-demand state: {state}"
    );

    let restored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(
        restored["enabledPlugins"]["pixijs@x"],
        serde_json::json!(false),
        "settings.local.json must be restored to its pre-acquire value"
    );
}

#[test]
fn doctor_without_fix_reports_but_writes_nothing() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let profiles = home.join(".claude").join("profiles").join("profiles.json");

    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("profiles.json"));

    assert!(!profiles.exists(), "doctor without --fix must not seed");
}

#[test]
fn doctor_fix_seeds_profiles_from_the_embedded_template() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();

    let profiles = home.join(".claude").join("profiles").join("profiles.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&profiles).unwrap()).unwrap();
    assert!(v.get("universal").is_some(), "seeded file: {v}");
}

#[test]
fn doctor_fix_never_overwrites_an_existing_profiles_json() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let dir = home.join(".claude").join("profiles");
    std::fs::create_dir_all(&dir).unwrap();
    let profiles = dir.join("profiles.json");
    std::fs::write(&profiles, r#"{"sentinel":"mine","profiles":{}}"#).unwrap();

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();

    assert!(std::fs::read_to_string(&profiles)
        .unwrap()
        .contains("sentinel"));
}

#[test]
fn doctor_fix_reports_stale_backups_but_leaves_them_on_disk() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let plugins = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let bak = plugins.join("installed_plugins.json.bak.1700000000");
    std::fs::write(&bak, "{}").unwrap();
    // The retired installer left timestamped backups beside BOTH files.
    let sbak = home.join(".claude").join("settings.json.bak.1700000001");
    std::fs::write(&sbak, "{}").unwrap();
    // profiles.json accumulates the same way, via write_profiles on every
    // board deploy / `profile init` — not from the retired installer, but
    // reclaimed by the same sweep.
    let profiles_dir = home.join(".claude").join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let pbak = profiles_dir.join("profiles.json.bak.1700000002");
    std::fs::write(&pbak, "{}").unwrap();

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--prune-backups"));

    assert!(bak.exists(), "--fix must not delete backups");
    assert!(sbak.exists(), "--fix must not delete backups");
    assert!(pbak.exists(), "--fix must not delete backups");
}

#[test]
fn doctor_errors_on_corrupt_profiles_json_with_and_without_fix() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let dir = home.join(".claude").join("profiles");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profiles.json"), "{not valid json").unwrap();

    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .failure()
        .stderr(predicate::str::contains("profiles.json"));

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("profiles.json"));
}

#[test]
fn doctor_errors_on_corrupt_registry_the_same_way_with_and_without_fix() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let plugins = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(plugins.join("installed_plugins.json"), "{not valid json").unwrap();

    // Same diagnosis whether or not --fix is passed: one tool must not give two
    // different verdicts about the same corrupt file.
    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .failure()
        .stderr(predicate::str::contains("installed_plugins.json"));

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("installed_plugins.json"));
}

#[test]
fn doctor_prune_backups_ignores_a_sidecar_with_a_non_numeric_suffix() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    // A user's own sidecar, not one the retired installer wrote.
    let user_sidecar = home
        .join(".claude")
        .join("settings.json.bak.before-migration");
    std::fs::write(&user_sidecar, "{}").unwrap();
    let stale = home.join(".claude").join("settings.json.bak.1700000001");
    std::fs::write(&stale, "{}").unwrap();

    cmd(home, ddir.path())
        .args(["doctor", "--fix", "--prune-backups"])
        .assert()
        .success();

    assert!(
        user_sidecar.exists(),
        "a non-epoch suffix must never be treated as one of ours"
    );
    assert!(
        !stale.exists(),
        "the epoch-suffixed sidecar is still reclaimed"
    );
}

#[test]
fn doctor_prints_stale_backup_paths_not_just_a_count() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let plugins = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let bak = plugins.join("installed_plugins.json.bak.1700000000");
    std::fs::write(&bak, "{}").unwrap();

    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "installed_plugins.json.bak.1700000000",
        ));
}

#[test]
fn doctor_prune_backups_removes_them() {
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();
    let plugins = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    let bak = plugins.join("installed_plugins.json.bak.1700000000");
    std::fs::write(&bak, "{}").unwrap();
    let sbak = home.join(".claude").join("settings.json.bak.1700000001");
    std::fs::write(&sbak, "{}").unwrap();
    let profiles_dir = home.join(".claude").join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let pbak = profiles_dir.join("profiles.json.bak.1700000002");
    std::fs::write(&pbak, "{}").unwrap();

    cmd(home, ddir.path())
        .args(["doctor", "--fix", "--prune-backups"])
        .assert()
        .success();

    assert!(!bak.exists(), "registry backups are reclaimed");
    assert!(!sbak.exists(), "settings backups are reclaimed too");
    assert!(!pbak.exists(), "profiles.json backups are reclaimed too");
}

#[test]
fn doctor_without_fix_says_theres_nothing_to_check_when_profiles_json_is_absent() {
    // M4: with no profiles.json at all, doctor never loaded anything to check
    // scope against. "plugin scope: already consistent" would be a diagnostic
    // reporting health it never established — the same anti-pattern C1 exists
    // to kill one level down.
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();

    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no profiles.json — nothing to check",
        ))
        .stdout(predicate::str::contains("already consistent").not());
}

#[test]
fn doctor_promotes_cc_loadouts_own_registry_key_even_though_no_profiles_json_names_it() {
    // This is the mitigation for the branch's one accepted residual risk: if
    // cc-loadout@cc-loadout itself drifts to scope: local, the plugin stops
    // resolving outside the repo it is bound to, and its own SessionStart hook
    // never runs there to repair it. `doctor`/`doctor --fix` are the only
    // recovery, so the self key must be treated as managed even though no
    // profiles.json — not even the seeded template — ever lists it.
    let hdir = tempfile::tempdir().unwrap();
    let ddir = tempfile::tempdir().unwrap();
    let home = hdir.path();

    let profiles_dir = home.join(".claude").join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("profiles.json"),
        r#"{"scan_roots":[],"universal":["other@x"],"profiles":{}}"#,
    )
    .unwrap();

    let plugins_dir = home.join(".claude").join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    let registry_path = plugins_dir.join("installed_plugins.json");
    std::fs::write(
        &registry_path,
        r#"{"version":2,"plugins":{"cc-loadout@cc-loadout":[{"scope":"local","lastUpdated":"1"}]}}"#,
    )
    .unwrap();

    cmd(home, ddir.path())
        .arg("doctor")
        .assert()
        .success()
        .stdout(predicate::str::contains("cc-loadout@cc-loadout"));

    cmd(home, ddir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();

    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    assert_eq!(
        v["plugins"]["cc-loadout@cc-loadout"][0]["scope"], "user",
        "the self key must be promoted even though profiles.json never names it"
    );
}
