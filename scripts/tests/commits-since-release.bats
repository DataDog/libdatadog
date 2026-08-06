#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# commits-since-release.sh decides which commits belong to each crate's release.
# Its output drives three later decisions in release-proposal-dispatch.yml:
#   * an empty `commits` array defers the crate to the libdd-* major-bump check
#     (i.e. it is dropped from the release unless a dependency forces it back in),
#   * `.commits[0]` / `.commits[-1]` become the git-cliff CHANGELOG range,
#   * `commits` is rendered verbatim into the PR body.
# A leaked or dropped commit therefore changes what gets released.

load helpers/common
load helpers/fixture

ALPHA_INPUT='[{"name":"libdd-alpha","version":"1.2.3"}]'

setup() {
  setup_common
  fixture_init
  # Pretend libdd-alpha 1.2.3 was released at the initial commit.
  fixture_tag "libdd-alpha-v1.2.3"
}

teardown() {
  teardown_common
}

@test "reports no previous release when the crate has no tag" {
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" '[{"name":"libdd-beta","version":"0.5.0"}]'
  assert_success
  assert_jq "$output" '.[0].tag_exists' "false"
  assert_jq "$output" '.[0].tag' "libdd-beta-v0.5.0"
  assert_jq "$output" '.[0].commits | length' "0"
}

@test "reports an empty commit list when nothing changed since the tag" {
  fixture_touch_crate libdd-beta "feat(beta): unrelated"
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].tag_exists' "true"
  assert_jq "$output" '.[0].commits | length' "0"
}

@test "collects only commits that touch the crate's directory" {
  fixture_touch_crate libdd-alpha "feat(alpha): mine"
  fixture_touch_crate libdd-beta "feat(beta): not mine"
  fixture_commit_all "chore: empty commit touching nothing"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | length' "1"
  assert_jq "$output" '.[0].commits[0].subject' "feat(alpha): mine"
}

@test "reports the crate path relative to the workspace root" {
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].path' "libdd-alpha"
}

@test "returns commits newest first" {
  # The CHANGELOG range is built as `.commits[-1]^..\.commits[0]`, so the order is
  # load-bearing: reversing it silently produces an empty or inverted range.
  fixture_touch_crate libdd-alpha "feat(alpha): oldest"
  fixture_touch_crate libdd-alpha "feat(alpha): middle"
  fixture_touch_crate libdd-alpha "feat(alpha): newest"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' \
    "feat(alpha): newest|feat(alpha): middle|feat(alpha): oldest"
}

@test "excludes merge commits and chore(release) commits by default" {
  fixture_touch_crate libdd-alpha "feat(alpha): keep me"
  fixture_touch_crate libdd-alpha "chore(release): update CHANGELOG.md for libdd-alpha"
  fixture_touch_crate libdd-alpha "Merge branch 'main' into topic"
  fixture_touch_crate libdd-alpha "Merge pull request #1 from someone/topic"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "feat(alpha): keep me"
}

@test "excludes commits authored by the release bot" {
  # Release automation commits must never be re-proposed as release content.
  fixture_touch_crate libdd-alpha "feat(alpha): human change"
  fixture_touch_crate libdd-alpha "feat(alpha): bot change" "dd-octo-sts[bot]" "bot@example.com"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "feat(alpha): human change"
}

@test "--no-exclude keeps subject-filtered commits but still drops the release bot" {
  fixture_touch_crate libdd-alpha "chore(release): bump"
  fixture_touch_crate libdd-alpha "feat(alpha): bot change" "dd-octo-sts[bot]" "bot@example.com"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" --no-exclude "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "chore(release): bump"
}

@test "--exclude adds a subject pattern on top of the defaults" {
  fixture_touch_crate libdd-alpha "feat(alpha): keep me"
  fixture_touch_crate libdd-alpha "ci: drop me"
  fixture_touch_crate libdd-alpha "chore(release): also dropped"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" --exclude='^ci:' "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "feat(alpha): keep me"
}

@test "produces valid JSON for commit subjects with quotes, backslashes and unicode" {
  # The script assembles JSON by string concatenation; only subject and author are
  # escaped. A regression here yields a malformed PR body or a hard jq failure
  # halfway through the release job.
  local nasty='fix(alpha): handle "quoted" \back\slash and — em dash $(whoami) `tick`'
  fixture_touch_crate libdd-alpha "$nasty"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_valid_json "$output"
  assert_jq "$output" '.[0].commits[0].subject' "$nasty"
}

@test "dereferences an annotated tag and still finds the commits after it" {
  # git merge-base does not dereference annotated tag objects consistently across
  # git versions, so the script resolves TAG^{} explicitly.
  fixture_touch_crate libdd-beta "feat(beta): released"
  fixture_tag "libdd-beta-v0.5.0" --annotated
  fixture_touch_crate libdd-beta "feat(beta): after the tag"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" '[{"name":"libdd-beta","version":"0.5.0"}]'
  assert_success
  assert_jq "$output" '.[0].tag_exists' "true"
  assert_jq "$output" '.[0].tag_ancestor' "true"
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "feat(beta): after the tag"
}

@test "falls back to the merge-base when the tag is not an ancestor of HEAD" {
  # Squash-merged release branches leave the tag on history that main never saw.
  local fork_point
  fork_point="$(git rev-parse HEAD)"
  fixture_touch_crate libdd-alpha "feat(alpha): on main after fork"

  git checkout -q -b sidebranch "$fork_point"
  fixture_touch_crate libdd-alpha "chore: release-only commit"
  git tag -f "libdd-alpha-v1.2.3" >/dev/null
  git checkout -q main

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" "$ALPHA_INPUT"
  assert_success
  assert_jq "$output" '.[0].tag_ancestor' "$fork_point"
  # The commit that only exists on the side branch must not be attributed to main.
  assert_jq "$output" '.[0].commits | map(.subject) | join("|")' "feat(alpha): on main after fork"
}

@test "reads the crate list from stdin" {
  fixture_touch_crate libdd-alpha "feat(alpha): via stdin"
  run --separate-stderr bash -c "printf '%s' '$ALPHA_INPUT' | '${SCRIPTS_DIR}/commits-since-release.sh'"
  assert_success
  assert_jq "$output" '.[0].commits[0].subject' "feat(alpha): via stdin"
}

@test "handles several crates in one invocation" {
  fixture_tag "libdd-beta-v0.5.0"
  fixture_touch_crate libdd-alpha "feat(alpha): a"
  fixture_touch_crate libdd-beta "feat(beta): b"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" \
    '[{"name":"libdd-alpha","version":"1.2.3"},{"name":"libdd-beta","version":"0.5.0"}]'
  assert_success
  assert_jq "$output" 'length' "2"
  assert_jq "$output" '.[0].commits[0].subject' "feat(alpha): a"
  assert_jq "$output" '.[1].commits[0].subject' "feat(beta): b"
}

@test "rejects invalid JSON input" {
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" 'not json'
  assert_failure 1
  assert_stderr_contains "Invalid JSON input"
}

@test "rejects an unknown option and an unknown format" {
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" --nope "$ALPHA_INPUT"
  assert_failure 1
  assert_stderr_contains "Unknown option: --nope"

  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" --format=yaml "$ALPHA_INPUT"
  assert_failure 1
  assert_stderr_contains "Unknown format: yaml"
}

@test "summary format lists the commits per crate" {
  fixture_touch_crate libdd-alpha "feat(alpha): summarised"
  run --separate-stderr "${SCRIPTS_DIR}/commits-since-release.sh" --format=summary "$ALPHA_INPUT"
  assert_success
  assert_contains "$output" "libdd-alpha v1.2.3"
  assert_contains "$output" "feat(alpha): summarised"
}
