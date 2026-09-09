#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# semver-level.sh picks the bump level (major/minor/patch) that
# release-proposal-dispatch.yml passes to `cargo release version -x`. It combines two
# tools whose output it parses textually:
#   * cargo-semver-checks, which has known false negatives at signature level, and
#   * cargo-public-api, whose "Changed items" section it normalizes before judging.
# The parsing IS the logic, so these tests replay recorded tool output through a
# `cargo` shim (helpers/cargo-stub/cargo) instead of building crates for real.

load helpers/common
load helpers/fixture

setup() {
  setup_common
  fixture_init
  fixture_tag "libdd-alpha-v1.2.3"
  # semver-level.sh runs `git fetch origin <baseline>`; give it a real remote so the
  # fetch is a no-op rather than an error.
  fixture_add_origin

  STUB_REAL_CARGO="$(command -v cargo)"
  export STUB_REAL_CARGO
  PATH="${TESTS_DIR}/helpers/cargo-stub:${PATH}"
  export PATH
  STUB_CALL_LOG="${TEST_TMP}/cargo-calls.log"
  export STUB_CALL_LOG
  : > "$STUB_CALL_LOG"

  # Default: both tools clean. Individual tests override.
  stub_semver_checks 0 ""
  stub_public_api 0 "$(public_api_diff '(none)' '(none)' '(none)')"
}

teardown() {
  teardown_common
}

# stub_semver_checks RC OUTPUT
stub_semver_checks() {
  STUB_SEMVER_CHECKS_RC="$1"
  export STUB_SEMVER_CHECKS_RC
  STUB_SEMVER_CHECKS_OUT="${TEST_TMP}/semver-checks.out"
  export STUB_SEMVER_CHECKS_OUT
  printf '%s\n' "$2" > "$STUB_SEMVER_CHECKS_OUT"
}

# stub_public_api RC OUTPUT
stub_public_api() {
  STUB_PUBLIC_API_RC="$1"
  export STUB_PUBLIC_API_RC
  STUB_PUBLIC_API_OUT="${TEST_TMP}/public-api.out"
  export STUB_PUBLIC_API_OUT
  printf '%s\n' "$2" > "$STUB_PUBLIC_API_OUT"
}

# public_api_diff REMOVED CHANGED ADDED — reproduce cargo-public-api's diff layout,
# including the `===` underlines the script's `grep -A 2` checks depend on.
public_api_diff() {
  cat <<EOF
Removed items from the public API
=================================
$1

Changed items in the public API
===============================
$2

Added items to the public API
=============================
$3
EOF
}

run_semver_level() {
  run_tool semver-level libdd-alpha "refs/tags/libdd-alpha-v1.2.3" HEAD
}

@test "no API change at all is a patch release" {
  # Every crate with commits gets released, so 'none' is deliberately floored to patch.
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "patch"
  assert_jq "$output" '.name' "libdd-alpha"
  assert_jq "$output" '.reason' "No public API changes detected"
}

@test "cargo-semver-checks reporting a breaking change is major" {
  stub_semver_checks 1 "$(cat <<'EOF'
--- failure struct_missing: pub struct removed or renamed ---
Description: A publicly-visible struct cannot be imported by its prior path.
     Summary semver requires new major version: 1 major and 0 minor checks failed
EOF
)"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "cargo-semver-checks detected breaking changes"
  assert_contains "$(jq -r '.details' <<< "$output")" "struct_missing"
}

@test "cargo-public-api is skipped once cargo-semver-checks already says major" {
  # It cannot raise the level any further, and it is the expensive half.
  stub_semver_checks 1 "     Summary semver requires new major version: 1 major and 0 minor checks failed"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "major"
  assert_not_contains "$(cat "$STUB_CALL_LOG")" "public-api"
}

@test "a crate missing from the baseline is a new crate and is treated as minor" {
  stub_semver_checks 1 "error: package \`libdd-alpha\` not found"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "New crate (not present in baseline)"
  # There is no baseline to diff against, so cargo-public-api must not run either.
  assert_not_contains "$(cat "$STUB_CALL_LOG")" "public-api"
}

@test "removed public items are major even when cargo-semver-checks is clean" {
  stub_public_api 0 "$(public_api_diff 'pub fn libdd_alpha::gone()' '(none)' '(none)')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "cargo-public-api detected removed public API items"
}

@test "added public items are minor" {
  stub_public_api 0 "$(public_api_diff '(none)' '(none)' 'pub fn libdd_alpha::added()')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "cargo-public-api detected new public API items"
}

@test "a changed signature is major — the case cargo-semver-checks misses" {
  # A parameter type change on a non-generic fn keeps the item path, so semver-checks
  # stays silent and only the -old/+new pair under "Changed items" reveals it.
  stub_public_api 0 "$(public_api_diff '(none)' \
    "$(printf -- '-pub fn libdd_alpha::convert(u32) -> u32\n+pub fn libdd_alpha::convert(u64) -> u32')" \
    '(none)')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "cargo-public-api detected breaking signature changes"
}

@test "a changed item that only gained an attribute is not breaking" {
  stub_public_api 0 "$(public_api_diff '(none)' \
    "$(printf -- '-pub struct libdd_alpha::Config\n+#[repr(C)] pub struct libdd_alpha::Config')" \
    '(none)')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "patch"
  assert_jq "$output" '.reason' "No public API changes detected"
}

@test "a changed item that only gained const/async/unsafe qualifiers is not breaking" {
  stub_public_api 0 "$(public_api_diff '(none)' \
    "$(printf -- '-pub fn libdd_alpha::build() -> Config\n+pub const fn libdd_alpha::build() -> Config')" \
    '(none)')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "patch"
}

@test "a non-breaking change alongside added items still reports minor" {
  stub_public_api 0 "$(public_api_diff '(none)' \
    "$(printf -- '-pub fn libdd_alpha::build() -> Config\n+pub const fn libdd_alpha::build() -> Config')" \
    'pub fn libdd_alpha::added()')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "minor"
}

@test "takes the higher of the two signals" {
  # cargo-semver-checks says minor, cargo-public-api says major: major wins, and the
  # reported reason comes from the tool that decided the level.
  stub_semver_checks 1 "     Summary semver requires new minor version: 0 major and 1 minor checks failed"
  stub_public_api 0 "$(public_api_diff 'pub fn libdd_alpha::gone()' '(none)' '(none)')"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "cargo-public-api detected removed public API items"
}

@test "keeps the cargo-semver-checks level when cargo-public-api finds less" {
  stub_semver_checks 1 "     Summary semver requires new minor version: 0 major and 1 minor checks failed"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "cargo-semver-checks detected minor breaking changes"
}

@test "fails loudly on an unrecognised cargo-semver-checks failure" {
  # Silently downgrading an unparsed failure to patch would ship a breaking change
  # as a patch release.
  stub_semver_checks 1 "error: could not compile \`libdd-alpha\`"
  run_semver_level
  assert_failure
  assert_stderr_mentions "cargo-semver-checks"
}

@test "fails on an unexpected cargo-semver-checks exit code" {
  stub_semver_checks 101 "thread 'main' panicked"
  run_semver_level
  assert_failure 101
  assert_stderr_mentions "cargo-semver-checks"
}

@test "fails when cargo-public-api errors out" {
  stub_public_api 1 "error: failed to build rustdoc JSON"
  run_semver_level
  assert_failure
  assert_stderr_mentions "cargo-public-api"
}

@test "passes the tag baseline through unprefixed and diffs baseline..current" {
  # The workflow calls this with refs/tags/<tag>. Prefixing that with origin/ (as the
  # script does for branch names) would resolve to nothing.
  run_semver_level
  assert_success
  local calls
  calls="$(cat "$STUB_CALL_LOG")"
  assert_contains "$calls" "semver-checks -p libdd-alpha --color=never --all-features --baseline-rev refs/tags/libdd-alpha-v1.2.3"
  assert_contains "$calls" "diff refs/tags/libdd-alpha-v1.2.3..HEAD"
}

@test "prefixes a bare branch baseline with origin/" {
  run_tool semver-level libdd-alpha main HEAD
  assert_success
  assert_contains "$(cat "$STUB_CALL_LOG")" "--baseline-rev origin/main"
}

@test "defaults the current ref to HEAD and requires a crate name" {
  run_tool semver-level libdd-alpha main
  assert_success
  assert_contains "$(cat "$STUB_CALL_LOG")" "diff origin/main..HEAD"

  run_tool semver-level
  assert_failure
  assert_stderr_mentions
}

@test "emits a single JSON object with the fields the workflow reads" {
  # The workflow does `jq -r '.level'` on this; extra chatter on stdout breaks it.
  run_semver_level
  assert_success
  assert_valid_json "$output"
  assert_jq "$output" 'keys | join(",")' "details,level,name,reason"
}
