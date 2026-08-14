#!/bin/sh
# Propagate the crate version onto the plugin's two version files.
#
# Cargo.toml is the single source of truth: release-plz bumps it (plus
# Cargo.lock and CHANGELOG.md) and touches nothing else. Two more files carry
# the same number for different reasons:
#
#   .claude-plugin/plugin.json   what Claude Code displays
#   .claude-plugin/cli-version   what scripts/launcher.sh downloads
#
# The pin is the dangerous one. launcher.sh resolves it to a GitHub Release
# tarball, so a stale pin means a user installs a new plugin and silently runs
# the old binary — the exact skew the pin exists to prevent. Hand-editing three
# files in step is how that drift gets introduced, so release.yml runs this on
# the release PR and ci.yml runs `--check` on every push.
#
#   scripts/sync-versions.sh           rewrite the two files to match Cargo.toml
#   scripts/sync-versions.sh --check   exit 1 if they already disagree
set -eu

check=0
case "${1:-}" in
'') ;;
--check) check=1 ;;
*)
	echo "usage: scripts/sync-versions.sh [--check]" >&2
	exit 2
	;;
esac

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cargo_toml="$ROOT/Cargo.toml"
plugin_json="$ROOT/.claude-plugin/plugin.json"
pin_file="$ROOT/.claude-plugin/cli-version"
market_json="$ROOT/.claude-plugin/marketplace.json"

# Only [package].version sits at column 0; every dependency's `version =` is
# indented inside an inline table, so the first column-0 match is unambiguous.
want=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$cargo_toml" | head -1)
case "$want" in
[0-9]*.[0-9]*.[0-9]*) ;;
*)
	echo "sync-versions: no package version in $cargo_toml (read '$want')" >&2
	exit 1
	;;
esac

plugin_now=$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$plugin_json" | head -1)
pin_now=$(tr -d '[:space:]' <"$pin_file" 2>/dev/null || true)

if [ "$check" -eq 1 ]; then
	rc=0
	if [ "$plugin_now" != "$want" ]; then
		echo "FAIL: plugin.json version '$plugin_now' != Cargo.toml '$want'" >&2
		rc=1
	fi
	if [ "$pin_now" != "$want" ]; then
		echo "FAIL: cli-version pin '$pin_now' != Cargo.toml '$want'" >&2
		rc=1
	fi
	# A version declared here silently overrides plugin.json, so all three files
	# above could agree and users would still resolve a fourth number.
	if grep -q '"version"' "$market_json"; then
		echo "FAIL: marketplace.json declares a version; plugin.json must own it" >&2
		rc=1
	fi
	if [ "$rc" -eq 0 ]; then
		echo "ok: Cargo.toml, plugin.json and cli-version all at $want"
	else
		echo "run scripts/sync-versions.sh to bring them back in line" >&2
	fi
	exit "$rc"
fi

if [ "$plugin_now" = "$want" ] && [ "$pin_now" = "$want" ]; then
	echo "already at $want — nothing to sync"
	exit 0
fi

# `sed -i` is not portable (GNU takes no argument, BSD requires one), and a
# targeted rewrite keeps plugin.json's hand-formatting where a jq round-trip
# would reflow the whole file.
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
sed 's/\("version"[[:space:]]*:[[:space:]]*\)"[^"]*"/\1"'"$want"'"/' "$plugin_json" >"$tmp"
if command -v jq >/dev/null 2>&1; then
	jq -e . "$tmp" >/dev/null || {
		echo "sync-versions: the rewrite would leave $plugin_json invalid" >&2
		exit 1
	}
fi
# `cat >` rather than `mv`: keeps the file's own mode instead of mktemp's 0600.
cat "$tmp" >"$plugin_json"

printf '%s\n' "$want" >"$pin_file"

echo "synced plugin.json  $plugin_now -> $want"
echo "synced cli-version  $pin_now -> $want"
