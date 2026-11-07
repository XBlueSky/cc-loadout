//! The sandbox every integration test builds the cc-loadout binary under test
//! with (shared by `tests/cli.rs` and `tests/collector.rs`).

use assert_cmd::Command;
use std::path::Path;

pub fn write_login(home: &Path, email: &str) {
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
/// It ALSO removes CC_LOADOUT_PROFILES (profiles_path() prefers it over $HOME, so a
/// value leaking in from the ambient shell would redirect profiles.json reads/writes
/// out of the sandbox) and CLAUDE_CODE_SESSION_ID (`cargo test` run from a Claude
/// Code Bash tool inherits the live session's id, which would let an on-demand test
/// pass on the ambient value instead of the one it sets). Do NOT call
/// `Command::cargo_bin("cc-loadout")` directly in any test file — always go
/// through `cmd()` and chain extra `.env(...)` / `.current_dir(...)` as needed. A
/// test that must inspect the table overrides PATH with its own
/// `fake_crontab_path(dir)` and reads that dir's `tab`. A test that wants
/// CC_LOADOUT_PROFILES or CLAUDE_CODE_SESSION_ID set does so explicitly after
/// construction — `.env()` after `.env_remove()` still wins.
pub fn cmd(home: &Path, data: &Path) -> Command {
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
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CC_LOADOUT_PROFILES")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    c
}

/// Install a file-backed fake `crontab` in `bin_dir` and return the PATH string to
/// hand the command (fake dir first). Unlike a `>/dev/null` stub, this persists the
/// table to `bin_dir/tab`, so the binary's write-then-read-back verification (which
/// guards against a `crontab` that silently swallows writes) round-trips instead of
/// failing — while still never touching the developer's real user crontab.
pub fn fake_crontab_path(bin_dir: &Path) -> String {
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
