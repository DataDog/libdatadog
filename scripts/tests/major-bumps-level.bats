#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# major-bumps-level.sh answers: "did a direct libdd-* dependency of this crate go to a
# new major version since its last release?" In release-proposal-dispatch.yml a
# non-empty `major_bumps` forces the crate to a major bump, and pulls crates with no
# commits of their own back into the release. A false positive ships an unnecessary
# major; a false negative ships a crate whose dependency broke underneath it.

load helpers/common
load helpers/fixture

# Build an api-changes.json row the way the workflow does.
row() {
  local name="$1" prev_tag="$2" level="${3:-patch}" initial="${4:-false}"
  jq -nc --arg name "$name" --arg prev_tag "$prev_tag" --arg level "$level" \
    --arg initial "$initial" \
    '{name: $name, level: $level, tag: "", prev_tag: $prev_tag, version: "0.0.0",
      range: "", commits: [], path: $name, initial_release: $initial}'
}

# Write rows to a file and run the script against it.
run_major_bumps() {
  printf '%s\n' "$@" | jq -s '.' > "${TEST_TMP}/api-changes.json"
  run_tool major-bumps-level "${TEST_TMP}/api-changes.json"
}

setup() {
  setup_common
  fixture_init
  fixture_tag "libdd-beta-v0.5.0"
  fixture_tag "libdd-gamma-v2.0.1"
  fixture_tag "libdd-delta-v0.1.0"
  fixture_tag "other-tool-v0.3.0"
}

teardown() {
  teardown_common
}

@test "records a direct libdd-* dependency that moved to a new major" {
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps | length' "1"
  assert_jq "$output" '.[0].major_bumps[0].dependency' "libdd-alpha"
  assert_jq "$output" '.[0].major_bumps[0].previous_req' "^1.2"
  assert_jq "$output" '.[0].major_bumps[0].current_req' "^2.0"
}

@test "ignores a minor bump of a direct dependency" {
  fixture_set_dep_req libdd-beta libdd-alpha "1.3"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "ignores an unchanged dependency" {
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "known limitation: a 0.x to 0.y bump is not treated as major" {
  # `first_num` compares only the leading integer of the requirement, so 0.5 -> 0.6
  # scores 0 vs 0. Under cargo's semver rules that IS a breaking change for a 0.x
  # dependency. This test pins the current behaviour so a change to it is deliberate.
  fixture_set_dep_req libdd-gamma libdd-beta "0.6"
  run_major_bumps "$(row libdd-gamma libdd-gamma-v2.0.1)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "ignores dependencies that are not libdd-*" {
  cat >> "${FIXTURE_REPO}/libdd-beta/Cargo.toml" <<'EOF'
other-tool = { path = "../other-tool", version = "0.3" }
EOF
  fixture_commit_all "chore: add a non-libdd dependency"
  git tag -f "libdd-beta-v0.5.0" >/dev/null

  fixture_set_dep_req libdd-beta other-tool "1.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "ignores dev-dependency and build-dependency major bumps" {
  # libdd-delta build-depends on libdd-beta and dev-depends on libdd-alpha. Neither
  # affects what a consumer of libdd-delta links against, so neither forces a major.
  fixture_set_dep_req libdd-delta libdd-beta "1.0"
  fixture_set_dep_req libdd-delta libdd-alpha "9.0"
  run_major_bumps "$(row libdd-delta libdd-delta-v0.1.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "prefers a concrete requirement over a wildcard when a dependency appears twice" {
  # A target-specific edge duplicates the dependency with req "*"; taking the wildcard
  # would make every comparison unparseable and hide real bumps.
  cat >> "${FIXTURE_REPO}/libdd-beta/Cargo.toml" <<'EOF'

[target.'cfg(unix)'.dependencies]
libdd-alpha = { path = "../libdd-alpha" }
EOF
  fixture_commit_all "chore: add a target-specific duplicate dependency"
  git tag -f "libdd-beta-v0.5.0" >/dev/null

  fixture_set_dep_req libdd-beta libdd-alpha "3.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps | length' "1"
  assert_jq "$output" '.[0].major_bumps[0].previous_req' "^1.2"
  assert_jq "$output" '.[0].major_bumps[0].current_req' "^3.0"
}

@test "ignores a dependency that did not exist at the previous tag" {
  cat >> "${FIXTURE_REPO}/other-tool/Cargo.toml" <<'EOF'
libdd-beta = { path = "../libdd-beta", version = "0.5" }
EOF
  run_major_bumps "$(row other-tool other-tool-v0.3.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "ignores a dependency that was removed since the previous tag" {
  sed -i '/^libdd-alpha = /d' "${FIXTURE_REPO}/libdd-beta/Cargo.toml"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "skips crates flagged as an initial release" {
  # There is no previous tag to diff against; the crate is new.
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta "" patch true)"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "skips crates with an empty or null prev_tag" {
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta "")"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"

  run_major_bumps "$(row libdd-beta "null")"
  assert_success
  assert_jq "$output" '.[0].major_bumps' "[]"
}

@test "passes the original row through unchanged apart from major_bumps" {
  # The workflow re-reads level, prev_tag, version, path and commits from this output.
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0 minor)"
  assert_success
  assert_jq "$output" '.[0].level' "minor"
  assert_jq "$output" '.[0].prev_tag' "libdd-beta-v0.5.0"
  assert_jq "$output" '.[0].path' "libdd-beta"
  assert_jq "$output" '.[0].initial_release' "false"
  assert_jq "$output" '.[0] | has("major_bumps")' "true"
}

@test "preserves extra fields such as pending_release" {
  # The workflow tags no-commit candidates with pending_release and relies on the
  # flag surviving the audit to decide whether to keep them out of the release.
  local pending
  pending="$(row libdd-beta libdd-beta-v0.5.0 none | jq -c '. + {pending_release: "true"}')"
  run_major_bumps "$pending"
  assert_success
  assert_jq "$output" '.[0].pending_release' "true"
}

@test "handles several crates and reports each independently" {
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)" "$(row libdd-gamma libdd-gamma-v2.0.1)"
  assert_success
  assert_jq "$output" 'length' "2"
  assert_jq "$output" '.[0].major_bumps | length' "1"
  assert_jq "$output" '.[1].major_bumps' "[]"
}

@test "reports the bump on stderr so the job log explains the forced major" {
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  run_major_bumps "$(row libdd-beta libdd-beta-v0.5.0)"
  assert_success
  assert_stderr_mentions "libdd-alpha" "^1.2" "^2.0"
}

@test "fails when the crate manifest is missing from the current tree" {
  run_major_bumps "$(row libdd-nonexistent libdd-beta-v0.5.0)"
  assert_failure
  assert_stderr_mentions "libdd-nonexistent"
}

@test "fails when the crate manifest is missing at the previous tag" {
  _fixture_crate libdd-epsilon 0.1.0
  sed -i 's|    "other-tool",|    "other-tool",\n    "libdd-epsilon",|' "${FIXTURE_REPO}/Cargo.toml"
  run_major_bumps "$(row libdd-epsilon libdd-beta-v0.5.0)"
  assert_failure
  assert_stderr_mentions "libdd-epsilon"
}

@test "rejects a missing or non-file argument" {
  run_tool major-bumps-level
  assert_failure
  assert_stderr_mentions

  run_tool major-bumps-level "${TEST_TMP}/does-not-exist.json"
  assert_failure
  assert_stderr_mentions "does-not-exist.json"
}

@test "returns an empty array for an empty input list" {
  echo '[]' > "${TEST_TMP}/api-changes.json"
  run_tool major-bumps-level "${TEST_TMP}/api-changes.json"
  assert_success
  assert_jq "$output" 'length' "0"
}
