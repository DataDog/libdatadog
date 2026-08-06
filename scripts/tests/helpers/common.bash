#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Shared assertions and paths for the scripts/tests bats suites.
# Loaded from every .bats file via `load helpers/common`.

# $status, $output and $stderr are set by bats' own `run`, not by this file.
# shellcheck disable=SC2154

# `run --separate-stderr` (used throughout so that $output stays parseable JSON even
# when a script logs progress to stderr) requires bats 1.5.0 or newer.
bats_require_minimum_version 1.5.0

# Absolute path to scripts/ (the directory under test) and to scripts/tests/.
TESTS_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." >/dev/null 2>&1 && pwd)"
export TESTS_DIR
SCRIPTS_DIR="$(cd -- "${TESTS_DIR}/.." >/dev/null 2>&1 && pwd)"
export SCRIPTS_DIR
REPO_ROOT="$(cd -- "${SCRIPTS_DIR}/.." >/dev/null 2>&1 && pwd)"
export REPO_ROOT

# Per-test scratch directory, removed by teardown_common.
setup_common() {
  TEST_TMP="$(mktemp -d "${BATS_TMPDIR:-/tmp}/libdd-scripts-test.XXXXXX")"
  export TEST_TMP
}

teardown_common() {
  if [[ -n "${TEST_TMP:-}" && -d "$TEST_TMP" ]]; then
    # Fixture repos may contain git worktrees registered elsewhere; plain rm is enough
    # because every worktree lives inside TEST_TMP too.
    chmod -R u+w "$TEST_TMP" 2>/dev/null || true
    rm -rf "$TEST_TMP"
  fi
}

# --- assertions -------------------------------------------------------------
#
# bats-assert is deliberately not vendored: these few helpers keep the suite to a
# single dependency (bats-core itself).

fail_with() {
  printf '%s\n' "$@" >&2
  return 1
}

assert_success() {
  if [[ "$status" -ne 0 ]]; then
    fail_with "expected success, got exit status $status" \
      "--- stdout ---" "$output" "--- stderr ---" "${stderr:-}"
  fi
}

assert_failure() {
  local expected="${1:-}"
  if [[ "$status" -eq 0 ]]; then
    fail_with "expected failure, got exit status 0" \
      "--- stdout ---" "$output" "--- stderr ---" "${stderr:-}"
  fi
  if [[ -n "$expected" && "$status" -ne "$expected" ]]; then
    fail_with "expected exit status $expected, got $status" \
      "--- stdout ---" "$output" "--- stderr ---" "${stderr:-}"
  fi
}

# Assert on the diagnostics a script writes to stderr. Tests use
# `run --separate-stderr`, so $output holds stdout only.
assert_stderr_contains() {
  assert_contains "${stderr:-}" "$1"
}

assert_eq() {
  local actual="$1" expected="$2"
  if [[ "$actual" != "$expected" ]]; then
    fail_with "values differ" "--- expected ---" "$expected" "--- actual ---" "$actual"
  fi
}

assert_contains() {
  local haystack="$1" needle="$2"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail_with "expected to find: $needle" "--- in ---" "$haystack"
  fi
}

assert_not_contains() {
  local haystack="$1" needle="$2"
  if [[ "$haystack" == *"$needle"* ]]; then
    fail_with "expected NOT to find: $needle" "--- in ---" "$haystack"
  fi
}

# assert_jq JSON FILTER EXPECTED — run `jq -r FILTER` over JSON and compare.
assert_jq() {
  local json="$1" filter="$2" expected="$3" actual
  if ! actual="$(jq -r "$filter" <<< "$json" 2>&1)"; then
    fail_with "jq failed for filter: $filter" "--- error ---" "$actual" "--- json ---" "$json"
  fi
  if [[ "$actual" != "$expected" ]]; then
    fail_with "jq filter: $filter" "--- expected ---" "$expected" "--- actual ---" "$actual" "--- json ---" "$json"
  fi
}

# Fail unless the string is parseable JSON. Guards the scripts that build JSON by
# string concatenation, where an unescaped commit subject can produce garbage.
assert_valid_json() {
  local json="$1" err
  if ! err="$(jq empty <<< "$json" 2>&1)"; then
    fail_with "not valid JSON: $err" "--- value ---" "$json"
  fi
}
