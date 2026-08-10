#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# release-version-major-bumps.sh decides the final shape of a release: which crates get
# forced to major because a direct libdd-* dependency went to a new major inside this
# same proposal, and which no-commit candidates are pulled back in or dropped entirely.
# Get it wrong and a crate ships a compatible-looking version against an incompatible
# dependency, or silently disappears from the release.
#
# Nothing is stubbed here. major-bumps-level.sh only reads cargo metadata in a worktree
# -- no compilation -- and cargo-release is fast, so the whole step runs for real and the
# versions asserted are the ones a release would land on.

load helpers/common
load helpers/fixture

setup() {
  setup_common
  if ! cargo release --version >/dev/null 2>&1; then
    skip "cargo-release is not installed (cargo install cargo-release)"
  fi
  fixture_init

  # State at the last release: beta, gamma and other-tool all depend on libdd-alpha ^1.2,
  # libdd-delta only build- and dev-depends on its siblings.
  fixture_tag "libdd-alpha-v1.2.3"
  fixture_tag "libdd-beta-v0.5.0"
  fixture_tag "libdd-gamma-v2.0.1"
  fixture_tag "libdd-delta-v0.1.0"
  fixture_tag "other-tool-v0.3.0"
  git checkout -q -b proposal

  simulate_previous_release
}

teardown() {
  teardown_common
}

# What the "Release version bumps" step would have left behind: libdd-alpha released at
# major (1.2.3 -> 2.0.0) and libdd-beta at minor (0.5.0 -> 0.6.0). Driven through
# cargo-release rather than by editing manifests, so dependents' requirements are
# rewritten the same way a real release rewrites them and the workspace still resolves.
# Committed, because the audit reads the proposal branch tip.
simulate_previous_release() {
  cargo release version -p libdd-alpha --allow-branch proposal -x major --no-confirm >/dev/null
  cargo release version -p libdd-beta --allow-branch proposal -x minor --no-confirm >/dev/null
  fixture_commit_all "chore: Release"
}

# row NAME LEVEL PREV_VERSION VERSION [pending]
row() {
  jq -nc --arg n "$1" --arg l "$2" --arg pv "$3" --arg v "$4" --argjson p "${5:-false}" \
    '{name: $n, level: $l, tag: ($n + "-v" + $v), prev_tag: ($n + "-v" + $pv),
      version: $v, range: "", commits: [], path: $n, initial_release: "false"}
     + (if $p then {pending_release: "true"} else {} end)'
}

# run_major_bumps ROW... — feed rows in and run the script over them.
run_major_bumps() {
  printf '%s\n' "$@" | jq -s '.' > "${TEST_TMP}/api-changes.json"
  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" \
    --api-changes "${TEST_TMP}/api-changes.json" \
    --out "${TEST_TMP}/api-changes-with-major-bumps.json" \
    --branch proposal
  RESULT="$(cat "${TEST_TMP}/api-changes-with-major-bumps.json" 2>/dev/null || echo 'null')"
}

crate_version() {
  cargo metadata --format-version=1 --no-deps \
    | jq -r --arg n "$1" '.packages[] | select(.name == $n) | .version'
}

# --- promoting --------------------------------------------------------------

@test "promotes a released crate below major when a direct dependency went major" {
  run_major_bumps "$(row libdd-beta minor 0.5.0 0.6.0)"
  assert_success
  assert_jq "$RESULT" 'length' "1"
  assert_jq "$RESULT" '.[0].level' "major"
  assert_jq "$RESULT" '.[0].version' "1.0.0"
  assert_jq "$RESULT" '.[0].tag' "libdd-beta-v1.0.0"
  assert_eq "$(crate_version libdd-beta)" "1.0.0"
  assert_contains "$output" "Bumping libdd-beta to major"
}

@test "records why the crate was promoted" {
  run_major_bumps "$(row libdd-beta minor 0.5.0 0.6.0)"
  assert_success
  assert_jq "$RESULT" '.[0].major_bumps[0].dependency' "libdd-alpha"
  assert_jq "$RESULT" '.[0].major_bumps[0].previous_req' "^1.2"
  assert_jq "$RESULT" '.[0].major_bumps[0].current_req' "^2.0"
}

@test "commits one version bump per promoted crate" {
  local before
  before="$(git rev-parse HEAD)"
  run_major_bumps "$(row libdd-beta minor 0.5.0 0.6.0)" "$(row libdd-gamma minor 2.0.1 2.1.0)"
  assert_success
  assert_eq "$(git rev-list --count "${before}..HEAD")" "2"
  assert_contains "$(git log -1 --format=%s)" "chore(release): update version for"
}

@test "leaves a crate already bumped to major alone" {
  # It cannot go higher, so the audit must not bump it a second time.
  run_major_bumps "$(row libdd-beta major 0.5.0 1.0.0)"
  assert_success
  assert_jq "$RESULT" '.[0].level' "major"
  assert_jq "$RESULT" '.[0].version' "1.0.0"
  assert_eq "$(crate_version libdd-beta)" "0.6.0"
  assert_contains "$output" "Skipping libdd-beta: already bumped at major level"
}

@test "leaves a crate with no libdd-* dependency alone" {
  # libdd-alpha is the dependency that moved; it depends on nothing itself.
  run_major_bumps "$(row libdd-alpha major 1.2.3 2.0.0)"
  assert_success
  assert_jq "$RESULT" '.[0].version' "2.0.0"
  assert_jq "$RESULT" '.[0].major_bumps' "[]"
  assert_eq "$(crate_version libdd-alpha)" "2.0.0"
}

@test "ignores dev-dependency and build-dependency major bumps" {
  # libdd-delta only build- and dev-depends on its siblings, so nothing a consumer of
  # libdd-delta links against changed.
  run_major_bumps "$(row libdd-delta minor 0.1.0 0.2.0)"
  assert_success
  assert_jq "$RESULT" '.[0].level' "minor"
  assert_jq "$RESULT" '.[0].major_bumps' "[]"
  assert_eq "$(crate_version libdd-delta)" "0.1.0"
}

# --- pending candidates -----------------------------------------------------

@test "pulls a pending crate into the release when its dependency went major" {
  run_major_bumps "$(row libdd-gamma none 2.0.1 2.0.1 true)"
  assert_success
  assert_jq "$RESULT" 'length' "1"
  assert_jq "$RESULT" '.[0].name' "libdd-gamma"
  assert_jq "$RESULT" '.[0].level' "major"
  assert_jq "$RESULT" '.[0].version' "3.0.0"
  assert_eq "$(crate_version libdd-gamma)" "3.0.0"
}

@test "drops a pending crate that earns no major bump" {
  # No commits of its own and nothing forcing it: it stays out of the release entirely.
  run_major_bumps "$(row libdd-delta none 0.1.0 0.1.0 true)"
  assert_success
  assert_jq "$RESULT" 'length' "0"
  assert_eq "$(crate_version libdd-delta)" "0.1.0"
  assert_contains "$output" "keeping it out of the release"
}

@test "never leaks pending_release into the result" {
  # Downstream steps treat every row in this file as a crate being released.
  run_major_bumps "$(row libdd-beta minor 0.5.0 0.6.0)" \
                  "$(row libdd-gamma none 2.0.1 2.0.1 true)" \
                  "$(row libdd-delta none 0.1.0 0.1.0 true)"
  assert_success
  assert_jq "$RESULT" 'any(.[]; has("pending_release"))' "false"
}

# --- ordering and batches ---------------------------------------------------

@test "handles a mixed batch, giving each crate its own outcome" {
  run_major_bumps "$(row libdd-alpha major 1.2.3 2.0.0)" \
                  "$(row libdd-beta minor 0.5.0 0.6.0)" \
                  "$(row libdd-gamma none 2.0.1 2.0.1 true)" \
                  "$(row libdd-delta none 0.1.0 0.1.0 true)"
  assert_success
  # Released crates keep their order; the promoted pending crate is appended after them.
  assert_jq "$RESULT" 'map("\(.name):\(.level):\(.version)") | join(" ")' \
    "libdd-alpha:major:2.0.0 libdd-beta:major:1.0.0 libdd-gamma:major:3.0.0"
}

@test "writes an empty array when there are no candidates" {
  run_major_bumps
  assert_success
  assert_jq "$RESULT" 'length' "0"
}

# --- hygiene and failure paths ----------------------------------------------

@test "removes the throwaway audit worktree" {
  # It is created inside the caller's repository; leaving it behind would make later
  # `git worktree add` calls in the same job collide.
  run_major_bumps "$(row libdd-beta minor 0.5.0 0.6.0)"
  assert_success
  assert_eq "$(git worktree list | tail -n +2 | wc -l)" "0"
}

@test "fails, with the audit output, when major-bumps-level.sh fails" {
  # A prev_tag that does not exist: the audit cannot check the crate out to compare
  # against. Failing loudly beats releasing on an unaudited row.
  run_major_bumps "$(row libdd-beta minor 9.9.9 0.6.0)"
  assert_failure
  assert_contains "$output" "Major bumps level script failed with code"
  # And it still cleaned up after itself.
  assert_eq "$(git worktree list | tail -n +2 | wc -l)" "0"
}

@test "fails when the jq that streams the audited rows fails" {
  # Regression: see release-version-bumps.bats. The audit itself succeeds here; the
  # failure is in the read of its output, which used to be a process substitution and
  # so exited 0 with nothing promoted.
  printf '%s\n' "$(row libdd-beta minor 0.5.0 0.6.0)" | jq -s '.' > "${TEST_TMP}/api-changes.json"

  use_failing_stream_jq
  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" \
    --api-changes "${TEST_TMP}/api-changes.json" \
    --out "${TEST_TMP}/api-changes-with-major-bumps.json" \
    --branch proposal
  assert_failure 5
  assert_stderr_contains "simulated failure on the streaming read"
  assert_eq "$(crate_version libdd-beta)" "0.6.0"
  # The audit worktree is still cleaned up on the way out.
  assert_eq "$(git worktree list | tail -n +2 | wc -l)" "0"
}

@test "rejects a missing or non-array input" {
  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" \
    --api-changes "${TEST_TMP}/nope.json" --out "${TEST_TMP}/out.json" --branch proposal
  assert_failure 1
  assert_stderr_contains "not a file"

  echo '{"not":"an array"}' > "${TEST_TMP}/bad.json"
  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" \
    --api-changes "${TEST_TMP}/bad.json" --out "${TEST_TMP}/out.json" --branch proposal
  assert_failure 1
  assert_stderr_contains "is not a JSON array"
}

@test "requires its three mandatory options" {
  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" --out /dev/null --branch b
  assert_failure 1
  assert_stderr_contains "--api-changes is required"

  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" --api-changes /dev/null --branch b
  assert_failure 1
  assert_stderr_contains "--out is required"

  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" --api-changes /dev/null --out /dev/null
  assert_failure 1
  assert_stderr_contains "--branch is required"

  run --separate-stderr "${SCRIPTS_DIR}/release-version-major-bumps.sh" --nope
  assert_failure 1
  assert_stderr_contains "Unknown option: --nope"
}
