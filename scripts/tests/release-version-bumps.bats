#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# release-version-bumps.sh turns commits-since-release.sh output into api-changes.json.
# It is where a crate's fate is decided: released at a level, deferred to the libdd-*
# major-bump check, skipped because a newer release already exists, or the whole run
# failed. Everything downstream -- the version bumps committed to the proposal branch,
# the CHANGELOGs, the PR body -- follows from what this produces.
#
# cargo-release runs for real against a throwaway workspace, so the versions asserted
# here are the ones a release would actually land on. Only semver-level.sh is stubbed:
# the real one builds two revisions of a crate with cargo-semver-checks and
# cargo-public-api, which is minutes per crate and orthogonal to what is under test.

load helpers/common
load helpers/fixture

setup() {
  setup_common
  if ! cargo release --version >/dev/null 2>&1; then
    skip "cargo-release is not installed (cargo install cargo-release)"
  fi
  fixture_init

  # libdd-alpha  1.2.3  tagged, tag is the latest, has commits  -> released
  # libdd-beta   0.5.0  tagged, but v0.9.0 also exists          -> skipped
  # libdd-gamma  2.0.1  tagged, no commits of its own           -> deferred
  # libdd-delta  0.1.0  never tagged                            -> initial release
  # libdd-private 0.9.0 never tagged, not 0.1.0                 -> hard error
  fixture_tag "libdd-alpha-v1.2.3"
  fixture_tag "libdd-beta-v0.5.0"
  fixture_tag "libdd-beta-v0.9.0"
  fixture_tag "libdd-gamma-v2.0.1"
  fixture_touch_crate libdd-alpha "feat(alpha): a change"
  fixture_touch_crate libdd-beta "feat(beta): a change"
  fixture_touch_crate libdd-delta "feat(delta): a change"
  git checkout -q -b proposal

  install_release_tool
}

teardown() {
  teardown_common
}

# Copy the script under test next to a semver-level.sh stub. The script resolves that
# sibling relative to itself, so no test-only hook is needed in the script.
install_release_tool() {
  RELEASE_BIN="${TEST_TMP}/bin"
  export RELEASE_BIN
  mkdir -p "$RELEASE_BIN"
  install -m 0755 "${SCRIPTS_DIR}/release-version-bumps.sh" "$RELEASE_BIN/"
  cat > "${RELEASE_BIN}/semver-level.sh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "${STUB_SEMVER_CALLS:-/dev/null}"
if [ "${STUB_SEMVER_RC:-0}" != "0" ]; then
  echo "cargo semver-checks blew up for $1" >&2
  exit "${STUB_SEMVER_RC}"
fi
jq -nc --arg n "$1" --arg l "${STUB_SEMVER_LEVEL:-minor}" \
  '{name: $n, level: $l, reason: "stub", details: ""}'
STUB
  chmod +x "${RELEASE_BIN}/semver-level.sh"
  STUB_SEMVER_CALLS="${TEST_TMP}/semver-level-calls"
  export STUB_SEMVER_CALLS
  : > "$STUB_SEMVER_CALLS"
}

# crates_input NAME:VERSION... — the shape publication-order.sh feeds downstream.
crates_input() {
  local spec
  for spec in "$@"; do
    jq -nc --arg n "${spec%%:*}" --arg v "${spec##*:}" '{name: $n, version: $v}'
  done | jq -s -c '.'
}

# run_bumps [--flags...] — resolve the crate list, then run the script over it.
run_bumps() {
  local -a flags=()
  while [[ "${1:-}" == --* ]]; do flags+=("$1"); shift; done
  "${SCRIPTS_DIR}/commits-since-release.sh" "$1" > "${TEST_TMP}/commits-by-crate.json"
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/commits-by-crate.json" \
    --out "${TEST_TMP}/api-changes.json" \
    --branch proposal "${flags[@]}"
  API_CHANGES="$(cat "${TEST_TMP}/api-changes.json" 2>/dev/null || echo 'null')"
}

crate_version() {
  cargo metadata --format-version=1 --no-deps \
    | jq -r --arg n "$1" '.packages[] | select(.name == $n) | .version'
}

# --- releasing --------------------------------------------------------------

@test "releases a tagged crate that has commits, at the level semver-level.sh reports" {
  STUB_SEMVER_LEVEL=minor run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  assert_jq "$API_CHANGES" 'length' "1"
  assert_jq "$API_CHANGES" '.[0].name' "libdd-alpha"
  assert_jq "$API_CHANGES" '.[0].level' "minor"
  assert_jq "$API_CHANGES" '.[0].version' "1.3.0"
  assert_jq "$API_CHANGES" '.[0].tag' "libdd-alpha-v1.3.0"
  assert_jq "$API_CHANGES" '.[0].prev_tag' "libdd-alpha-v1.2.3"
  assert_jq "$API_CHANGES" '.[0].initial_release' "false"
  assert_jq "$API_CHANGES" '.[0].commits | map(.subject) | join("|")' "feat(alpha): a change"
  # The manifest really was rewritten, not just the JSON.
  assert_eq "$(crate_version libdd-alpha)" "1.3.0"
}

@test "a major level bumps the major version" {
  STUB_SEMVER_LEVEL=major run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  assert_jq "$API_CHANGES" '.[0].level' "major"
  assert_jq "$API_CHANGES" '.[0].version' "2.0.0"
}

@test "a patch level bumps the patch version" {
  STUB_SEMVER_LEVEL=patch run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  assert_jq "$API_CHANGES" '.[0].level' "patch"
  assert_jq "$API_CHANGES" '.[0].version' "1.2.4"
}

@test "commits the version bump to the proposal branch" {
  local before
  before="$(git rev-parse HEAD)"
  STUB_SEMVER_LEVEL=minor run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  assert_eq "$(git rev-list --count "${before}..HEAD")" "1"
  assert_contains "$(git log -1 --format=%s)" "Release"
}

@test "carries the range through from commits-since-release.sh" {
  # The range is what git-cliff falls back to when generating the CHANGELOG.
  STUB_SEMVER_LEVEL=minor run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  local expected
  expected="$(jq -r '.[0].range' "${TEST_TMP}/commits-by-crate.json")"
  assert_jq "$API_CHANGES" '.[0].range' "$expected"
}

# --- deferring --------------------------------------------------------------

@test "defers a tagged crate with no commits of its own" {
  run_bumps "$(crates_input libdd-gamma:2.0.1)"
  assert_success
  assert_jq "$API_CHANGES" '.[0].pending_release' "true"
  assert_jq "$API_CHANGES" '.[0].level' "none"
  assert_jq "$API_CHANGES" '.[0].version' "2.0.1"
  assert_jq "$API_CHANGES" '.[0].commits' "[]"
  assert_jq "$API_CHANGES" '.[0].range' ""
  # Deferred means untouched: no bump, no commit, and semver-level.sh never ran.
  assert_eq "$(crate_version libdd-gamma)" "2.0.1"
  assert_eq "$(cat "$STUB_SEMVER_CALLS")" ""
}

# --- skipping ---------------------------------------------------------------

@test "skips a crate whose tag is not the latest for that crate" {
  # libdd-beta-v0.9.0 exists, so a newer release was already cut elsewhere.
  run_bumps "$(crates_input libdd-beta:0.5.0)"
  assert_success
  assert_jq "$API_CHANGES" 'length' "0"
  assert_eq "$(crate_version libdd-beta)" "0.5.0"
  assert_contains "$output" "Skipping release for libdd-beta"
}

@test "--hotfix releases a crate whose tag is not the latest" {
  STUB_SEMVER_LEVEL=patch run_bumps --hotfix "$(crates_input libdd-beta:0.5.0)"
  assert_success
  assert_jq "$API_CHANGES" 'length' "1"
  assert_jq "$API_CHANGES" '.[0].version' "0.5.1"
  assert_contains "$output" "because it is a hotfix"
}

@test "--bypass-standard-checks releases a crate whose tag is not the latest" {
  STUB_SEMVER_LEVEL=patch run_bumps --bypass-standard-checks "$(crates_input libdd-beta:0.5.0)"
  assert_success
  assert_jq "$API_CHANGES" 'length' "1"
  assert_jq "$API_CHANGES" '.[0].version' "0.5.1"
  assert_contains "$output" "because bypass_standard_checks is true"
}

# --- initial releases -------------------------------------------------------

@test "treats a crate with no tag as an initial release" {
  run_bumps "$(crates_input libdd-delta:0.1.0)"
  assert_success
  assert_jq "$API_CHANGES" '.[0].initial_release' "true"
  assert_jq "$API_CHANGES" '.[0].level' "major"
  assert_jq "$API_CHANGES" '.[0].prev_tag' ""
  assert_jq "$API_CHANGES" '.[0].range' ""
  assert_jq "$API_CHANGES" '.[0].version' "1.0.0"
  assert_jq "$API_CHANGES" '.[0].tag' "libdd-delta-v1.0.0"
  # An initial release has no baseline to diff against.
  assert_eq "$(cat "$STUB_SEMVER_CALLS")" ""
}

@test "refuses an untagged crate that is not at 0.1.0" {
  # A missing tag on a crate that is not brand new means something is wrong with the
  # tags, not that the crate is new; releasing it as 1.0.0 would be wrong.
  run_bumps "$(crates_input libdd-private:0.9.0)"
  assert_failure 1
  assert_stderr_contains "libdd-private is not a 0.1.0 release"
}

# --- failure paths ----------------------------------------------------------

@test "fails, with the reason, when semver-level.sh fails" {
  # The output used to be captured into a variable that errexit then discarded, so the
  # run aborted with nothing in the log.
  STUB_SEMVER_RC=3 run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_failure
  assert_stderr_contains "semver-level.sh failed for libdd-alpha"
  assert_stderr_contains "cargo semver-checks blew up"
}

@test "fails when a tagged crate has no usable range" {
  # tag_commit/range empty means commits-since-release.sh could not resolve the tag.
  "${SCRIPTS_DIR}/commits-since-release.sh" "$(crates_input libdd-alpha:1.2.3)" \
    | jq '[.[] | .range = "" | .tag_commit = ""]' > "${TEST_TMP}/commits-by-crate.json"
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/commits-by-crate.json" \
    --out "${TEST_TMP}/api-changes.json" --branch proposal
  assert_failure 1
  assert_stderr_contains "Could not dereference tag libdd-alpha-v1.2.3 to a commit"
}

@test "fails when the jq that streams the rows fails" {
  # Regression: this loop used to read from `done < <(jq -c '.[]' ...)`. A process
  # substitution's exit status is reported by neither set -e nor pipefail, so a jq
  # that died mid-stream left the loop with no input and the script exited 0 having
  # released nothing at all.
  "${SCRIPTS_DIR}/commits-since-release.sh" "$(crates_input libdd-alpha:1.2.3)" \
    > "${TEST_TMP}/commits-by-crate.json"

  use_failing_stream_jq
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/commits-by-crate.json" \
    --out "${TEST_TMP}/api-changes.json" \
    --branch proposal
  assert_failure 5
  assert_stderr_contains "simulated failure on the streaming read"
  # Nothing was released on the way out.
  assert_eq "$(crate_version libdd-alpha)" "1.2.3"
}

@test "rejects a missing, unreadable or non-array input" {
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/nope.json" --out "${TEST_TMP}/out.json" --branch proposal
  assert_failure 1
  assert_stderr_contains "not a file"

  echo '{"not":"an array"}' > "${TEST_TMP}/bad.json"
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/bad.json" --out "${TEST_TMP}/out.json" --branch proposal
  assert_failure 1
  assert_stderr_contains "is not a JSON array"
}

@test "requires its three mandatory options" {
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" --out /dev/null --branch b
  assert_failure 1
  assert_stderr_contains "--commits-by-crate is required"

  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" --commits-by-crate /dev/null --branch b
  assert_failure 1
  assert_stderr_contains "--out is required"

  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" --commits-by-crate /dev/null --out /dev/null
  assert_failure 1
  assert_stderr_contains "--branch is required"

  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" --nope
  assert_failure 1
  assert_stderr_contains "Unknown option: --nope"
}

# --- several crates at once -------------------------------------------------

@test "handles a mixed batch, giving each crate its own outcome" {
  # One release, one skip, one defer, one initial release, in publication order.
  STUB_SEMVER_LEVEL=minor run_bumps \
    "$(crates_input libdd-alpha:1.2.3 libdd-beta:0.5.0 libdd-gamma:2.0.1 libdd-delta:0.1.0)"
  assert_success
  assert_jq "$API_CHANGES" 'map(.name) | join(",")' "libdd-alpha,libdd-gamma,libdd-delta"
  assert_jq "$API_CHANGES" '.[] | select(.name == "libdd-alpha") | .level' "minor"
  assert_jq "$API_CHANGES" '.[] | select(.name == "libdd-gamma") | .pending_release' "true"
  assert_jq "$API_CHANGES" '.[] | select(.name == "libdd-delta") | .initial_release' "true"
  # Only the crate that needed a level asked for one.
  assert_eq "$(cat "$STUB_SEMVER_CALLS")" "libdd-alpha"
}

@test "writes an empty array when there are no candidates" {
  echo '[]' > "${TEST_TMP}/commits-by-crate.json"
  run --separate-stderr "${RELEASE_BIN}/release-version-bumps.sh" \
    --commits-by-crate "${TEST_TMP}/commits-by-crate.json" \
    --out "${TEST_TMP}/api-changes.json" --branch proposal
  assert_success
  assert_jq "$(cat "${TEST_TMP}/api-changes.json")" 'length' "0"
}

@test "warns when the tagged commit is not in any local branch" {
  # Re-tag onto history no branch points at -- what a squash-merged release leaves --
  # while keeping the crate's own commits after the fork point so it still gets released.
  local tag_point
  tag_point="$(git rev-parse 'libdd-alpha-v1.2.3^{}')"
  git checkout -q -b throwaway "$tag_point"
  fixture_touch_crate libdd-alpha "chore: release-only commit"
  git tag -f "libdd-alpha-v1.2.3" >/dev/null
  git checkout -q proposal
  git branch -qD throwaway

  STUB_SEMVER_LEVEL=patch run_bumps "$(crates_input libdd-alpha:1.2.3)"
  assert_success
  assert_jq "$API_CHANGES" 'length' "1"
  assert_contains "$output" "is not in any local branch"
}
