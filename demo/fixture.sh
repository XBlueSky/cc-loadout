#!/usr/bin/env bash
# Build the demo sandbox at <repo>/target/demo-fixture — a fake $HOME +
# $XDG_DATA_HOME for the README recording. Everything is fabricated
# (@example.com); nothing personal can enter the frame. Wipes and rebuilds
# on every run. Manual poking:
#   demo/fixture.sh
#   HOME=$PWD/target/demo-fixture XDG_DATA_HOME=$PWD/target/demo-fixture/.local/share \
#     PATH="$PWD/target/demo-fixture/bin:$PATH" cc-loadout
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fx="$repo/target/demo-fixture"
bin="$repo/target/release/cc-loadout"
[ -x "$bin" ] || { echo "error: build first: cargo build --release" >&2; exit 1; }

rm -rf "$fx"
mkdir -p "$fx"/{bin,repos} "$fx/.claude/plugins/marketplaces" "$fx/.claude/profiles" \
  "$fx/.local/share/cc-loadout/accounts"/{work,personal}

now=$(date +%s)
h=3600

# --- live login: active account = work, token healthy for ~3h ---------------
cat > "$fx/.claude.json" <<EOF
{ "oauthAccount": { "emailAddress": "work@example.com",
    "organizationUuid": "org-acme", "organizationName": "Acme Inc." } }
EOF
cat > "$fx/.claude/.credentials.json" <<EOF
{ "claudeAiOauth": { "accessToken": "demo-access", "refreshToken": "demo-refresh",
    "expiresAt": $(( (now + 3*h) * 1000 )) } }
EOF

# --- account snapshots -------------------------------------------------------
acc="$fx/.local/share/cc-loadout/accounts"
cp "$fx/.claude/.credentials.json" "$acc/work/credentials.json"
cat > "$acc/work/oauth.json" <<EOF
{ "emailAddress": "work@example.com", "organizationUuid": "org-acme", "organizationName": "Acme Inc." }
EOF
cat > "$acc/personal/credentials.json" <<EOF
{ "claudeAiOauth": { "accessToken": "demo-access-2", "refreshToken": "demo-refresh-2",
    "expiresAt": $(( (now + 2*h) * 1000 )) } }
EOF
cat > "$acc/personal/oauth.json" <<EOF
{ "emailAddress": "personal@example.com", "organizationUuid": "", "organizationName": "" }
EOF

# --- account registry: work active (window opened ~1h ago), personal primed --
cat > "$fx/.local/share/cc-loadout/state.json" <<EOF
{
  "version": 1,
  "active_alias": "work",
  "accounts": {
    "work": {
      "email": "work@example.com", "org_uuid": "org-acme", "org_name": "Acme Inc.",
      "added_at": $(( now - 30*24*h )), "last_used": $(( now - 1*h )), "last_primed": null
    },
    "personal": {
      "email": "personal@example.com", "org_uuid": "", "org_name": "",
      "added_at": $(( now - 30*24*h )), "last_used": $(( now - 6*h )), "last_primed": $(( now - 2*h ))
    }
  }
}
EOF

# --- installed plugins + marketplace descriptions ----------------------------
cat > "$fx/.claude/plugins/installed_plugins.json" <<EOF
{
  "plugins": {
    "serena@essentials":      [{ "scope": "user" }],
    "code-review@essentials": [{ "scope": "user" }],
    "rust-analyzer-pro@lang": [{ "scope": "user" }],
    "crate-docs@lang":        [{ "scope": "user" }],
    "vue-devkit@web":         [{ "scope": "user" }],
    "eslint-suite@web":       [{ "scope": "user" }],
    "ui-polish@web":          [{ "scope": "user" }],
    "rag-toolkit@ml":         [{ "scope": "user" }],
    "prompt-lab@ml":          [{ "scope": "user" }],
    "profiler@perf":          [{ "scope": "user" }]
  }
}
EOF

mkt() { # mkt <name> <json>
  mkdir -p "$fx/.claude/plugins/marketplaces/$1/.claude-plugin"
  printf '%s\n' "$2" > "$fx/.claude/plugins/marketplaces/$1/.claude-plugin/marketplace.json"
}
mkt essentials '{ "name": "essentials", "plugins": [
  { "name": "serena", "description": "Semantic code navigation" },
  { "name": "code-review", "description": "Review checklists & diff insights" } ] }'
mkt lang '{ "name": "lang", "plugins": [
  { "name": "rust-analyzer-pro", "description": "Rust LSP power tools" },
  { "name": "crate-docs", "description": "Crate docs at your fingertips" } ] }'
mkt web '{ "name": "web", "plugins": [
  { "name": "vue-devkit", "description": "Vue components & devtools" },
  { "name": "eslint-suite", "description": "Lint & format presets" },
  { "name": "ui-polish", "description": "Design-system helpers" } ] }'
mkt ml '{ "name": "ml", "plugins": [
  { "name": "rag-toolkit", "description": "Retrieval-augmented context" },
  { "name": "prompt-lab", "description": "Prompt iteration workbench" } ] }'
mkt perf '{ "name": "perf", "plugins": [
  { "name": "profiler", "description": "Flame graphs on demand" } ] }'

# --- profiles (every installed plugin assigned -> no review badge) ------------
cat > "$fx/.claude/profiles/profiles.json" <<EOF
{
  "scan_roots": ["$fx/repos"],
  "universal": ["serena@essentials", "code-review@essentials"],
  "profiles": {
    "rust": {
      "plugins": ["rust-analyzer-pro@lang", "crate-docs@lang"],
      "detect": { "marker_files": ["Cargo.toml"] }
    },
    "frontend": {
      "plugins": ["vue-devkit@web", "eslint-suite@web", "ui-polish@web"],
      "detect": { "marker_globs": ["*.vue"] }
    },
    "ai": {
      "plugins": ["rag-toolkit@ml", "prompt-lab@ml"],
      "detect": { "content": [{ "file": "requirements.txt", "word": "torch" }] }
    }
  },
  "on_demand": ["profiler@perf"]
}
EOF

# --- fake repos under the scan root -------------------------------------------
mkrepo() { mkdir -p "$fx/repos/$1"; git -C "$fx/repos/$1" init -q; }
mkrepo api-server
printf '[package]\nname = "api-server"\nversion = "0.1.0"\n' > "$fx/repos/api-server/Cargo.toml"
mkrepo cli-tools
printf '[package]\nname = "cli-tools"\nversion = "0.1.0"\n' > "$fx/repos/cli-tools/Cargo.toml"
mkrepo web-dashboard
printf '{ "name": "web-dashboard", "dependencies": { "vue": "^3.4.0" } }\n' > "$fx/repos/web-dashboard/package.json"
mkdir -p "$fx/repos/web-dashboard/src"
printf '<template><div /></template>\n' > "$fx/repos/web-dashboard/src/App.vue"
mkrepo ml-pipeline
printf 'torch==2.3.0\nnumpy\n' > "$fx/repos/ml-pipeline/requirements.txt"

# --- scan cache: board shows live counts without pressing s -------------------
cat > "$fx/.local/share/cc-loadout/scan-cache.json" <<EOF
{
  "roots": ["$fx/repos"],
  "repos": [
    { "path": "$fx/repos/api-server", "marker_files": ["Cargo.toml"],
      "marker_globs": [], "package_json_deps": [], "languages": ["rs"] },
    { "path": "$fx/repos/cli-tools", "marker_files": ["Cargo.toml"],
      "marker_globs": [], "package_json_deps": [], "languages": ["rs"] },
    { "path": "$fx/repos/web-dashboard", "marker_files": ["package.json"],
      "marker_globs": ["*.vue"], "package_json_deps": ["vue"], "languages": ["vue"] },
    { "path": "$fx/repos/ml-pipeline", "marker_files": ["requirements.txt"],
      "marker_globs": [], "package_json_deps": [], "languages": ["py"] }
  ],
  "uncovered": [],
  "scanned_at": $(( now - 120 ))
}
EOF

# --- schedule installed through a sandbox crontab stub kept for the recording -
# The Schedule tab flags drift when the live crontab's managed block differs from
# what tasks.json would generate. That derived block embeds a PATH built from
# cc-loadout's dir + `which claude` + standard dirs (src/task/ops.rs cron_context),
# so the block the stub STORES only matches what the TUI RE-DERIVES at record time
# if both run under the same PATH. Hence:
#   * RECORD_PATH is byte-identical to demo.tape's `Env PATH` — keep them in sync.
#   * the stub crontab is LEFT in place (first on PATH) so the TUI queries it, not
#     the host's real, empty crontab at /usr/bin/crontab (which would read as drift).
# Result: stored block == re-derived block => no drift banner in frame.
RECORD_PATH="$fx/bin:/usr/local/bin:/usr/bin:/bin"
cat > "$fx/bin/crontab" <<'EOF'
#!/usr/bin/env bash
# Sandbox crontab: persists to the fixture, never touches real cron.
f="$(cd "$(dirname "$0")/.." && pwd)/crontab.txt"
case "${1:-}" in
  -l) if [ -f "$f" ]; then cat "$f"; else exit 1; fi ;;
  -)  cat > "$f" ;;
  *)  exit 0 ;;
esac
EOF
chmod +x "$fx/bin/crontab"
ln -s "$bin" "$fx/bin/cc-loadout"

# PATH pinned to RECORD_PATH (the recording env), NOT the inherited PATH, so the
# cron block derived here matches the one derived at record time (see above).
sandbox() { HOME="$fx" XDG_DATA_HOME="$fx/.local/share" PATH="$RECORD_PATH" "$@"; }
sandbox "$bin" account schedule set personal 06:00 11:00 16:00 > /dev/null

# --- apply in one repo so Overview's PROFILE section shows "N applied" --------
( cd "$fx/repos/api-server" && sandbox "$bin" profile apply > /dev/null )

echo "$fx"
