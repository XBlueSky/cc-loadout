# shellcheck shell=bash

# Tally for the running test file
TEST_PASS=0
TEST_FAIL=0
TEST_NAME=""

assert_eq() {
  local actual="$1"; local expected="$2"; local msg="${3:-}"
  if [[ "$actual" == "$expected" ]]; then
    TEST_PASS=$((TEST_PASS+1))
    echo "  ok: ${msg:-assert_eq}"
  else
    TEST_FAIL=$((TEST_FAIL+1))
    echo "  FAIL: ${msg:-assert_eq}"
    echo "    expected: $expected"
    echo "    actual:   $actual"
  fi
}

assert_contains() {
  local haystack="$1"; local needle="$2"; local msg="${3:-}"
  if [[ "$haystack" == *"$needle"* ]]; then
    TEST_PASS=$((TEST_PASS+1))
    echo "  ok: ${msg:-assert_contains}"
  else
    TEST_FAIL=$((TEST_FAIL+1))
    echo "  FAIL: ${msg:-assert_contains}"
    echo "    needle:   $needle"
    echo "    haystack: $haystack"
  fi
}

# Make a fixture repo at $1; caller sets up files under it.
make_repo() {
  local dir; dir="$(mktemp -d)"
  ( cd "$dir" && git init -q )
  echo "$dir"
}

cleanup_repo() { rm -rf "$1"; }

# Use a tiny test profiles.json so tests are independent of real config.
TEST_PROFILES_JSON="$(mktemp)"
cat > "$TEST_PROFILES_JSON" <<'JSON'
{
  "scan_roots": ["/tmp/ccloadout-fixtures"],
  "universal": ["univ-a@m1", "univ-b@m1"],
  "profiles": {
    "backend": {
      "plugins": ["be-a@m1"],
      "detect": {
        "path_prefixes": ["/tmp/ccloadout-fixtures/be/"],
        "marker_files": ["BUILD.bazel"]
      }
    },
    "frontend": {
      "plugins": ["fe-a@m1"],
      "detect": {
        "marker_globs": ["*.vue"],
        "package_json_deps": ["vue", "react"]
      }
    },
    "plugin-dev": {
      "plugins": ["pd-a@m1"],
      "detect": {
        "marker_files": ["plugin.json"]
      }
    },
    "ai-side": {
      "plugins": ["ai-a@m1"],
      "detect": {
        "marker_files": ["requirements.txt"],
        "deps_keywords": ["langchain"]
      }
    }
  }
}
JSON
export CC_LOADOUT_PROFILES="$TEST_PROFILES_JSON"

trap 'rm -f "$TEST_PROFILES_JSON"' EXIT
