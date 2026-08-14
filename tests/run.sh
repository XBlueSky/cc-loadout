#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

total_pass=0
total_fail=0
# The bash suite covers the installer, the plugin manifest and the plugin's
# binary launcher; hook and registry behaviour live in `cargo test`.
for f in tests/test_install.sh tests/test_plugin_manifest.sh tests/test_launcher.sh; do
  [[ -f "$f" ]] || continue
  echo "=== $f ==="
  TEST_PASS=0; TEST_FAIL=0
  # shellcheck source=/dev/null
  source "$f"
  total_pass=$((total_pass + TEST_PASS))
  total_fail=$((total_fail + TEST_FAIL))
done

echo
echo "Total: $total_pass passed, $total_fail failed"
[[ $total_fail -eq 0 ]] || exit 1
