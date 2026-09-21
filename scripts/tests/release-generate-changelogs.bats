#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# release-generate-changelogs.sh writes the CHANGELOG.md entries that ship with the
# release. Unlike the version bumps, a mistake here is not caught by anything
# downstream -- a wrong or missing entry is published and stays published.
#
# git-cliff runs for real, against the repository's own cliff.toml, so the rendered
# output is what a release would actually publish.

load helpers/common
load helpers/fixture

setup() {
  setup_common
  if ! git cliff --version >/dev/null 2>&1; then
    skip "git-cliff is not installed (cargo install git-cliff)"
  fi
  fixture_init
  fixture_add_cliff_config
  git checkout -q -b proposal
  RELEASE_DATE="$(date +%Y-%m-%d)"
}

teardown() {
  teardown_common
}

# row NAME VERSION PREV_TAG TAG COMMITS_JSON MAJOR_BUMPS_JSON INITIAL_RELEASE
row() {
  jq -nc --arg n "$1" --arg v "$2" --arg pt "$3" --arg t "$4" \
     --argjson c "$5" --argjson mb "$6" --arg ir "$7" \
    '{name: $n, level: "minor", tag: $t, prev_tag: $pt, version: $v, range: "",
      commits: $c, path: $n, initial_release: $ir, major_bumps: $mb}'
}

run_changelogs() {
  local -a flags=()
  while [[ "${1:-}" == --* ]]; do flags+=("$1" "$2"); shift 2; done
  printf '%s\n' "$@" | jq -s '.' > "${TEST_TMP}/api-changes.json"
  run_tool release-generate-changelogs \
    --api-changes "${TEST_TMP}/api-changes.json" "${flags[@]}"
}

seed_changelog() {
  printf '# Changelog\n\n\n## [%s] - 2020-01-01\n\n### Added\n\n- an older release\n' "$2" \
    > "${FIXTURE_REPO}/$1/CHANGELOG.md"
  fixture_commit_all "chore: seed $1 changelog"
}

changelog() { cat "${FIXTURE_REPO}/$1/CHANGELOG.md"; }
release_commits() { git log --format='%s' proposal --not main; }

MB='[{"dependency":"libdd-core","previous_req":"^1.0","current_req":"^2.0"}]'

# --- the git-cliff path -----------------------------------------------------

@test "renders an entry from the crate's commits" {
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): add a thing"
  fixture_touch_crate libdd-alpha "fix(alpha): fix a thing"
  local commits
  commits="$(fixture_commits_json HEAD~2..HEAD -- libdd-alpha)"

  run_changelogs "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)"
  assert_success
  local out; out="$(changelog libdd-alpha)"
  assert_contains "$out" "Add a thing"
  assert_contains "$out" "Fix a thing"
  # Conventional-commit types become the sections cliff.toml defines.
  assert_contains "$out" "### Added"
  assert_contains "$out" "### Fixed"
}

@test "links the new version to a compare view against the previous tag" {
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): add a thing"
  local commits; commits="$(fixture_commits_json HEAD~1..HEAD -- libdd-alpha)"

  run_changelogs "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)"
  assert_success
  assert_contains "$(changelog libdd-alpha)" \
    "compare/libdd-alpha-v1.0.0..libdd-alpha-v1.1.0"
}

@test "includes only the crate's own commits, not everything in the range" {
  # This is why the generation is two-pass. git-cliff's own path filter works on
  # cumulative tree diffs, so an unrelated commit sitting between the oldest and
  # newest of this crate's commits would otherwise be swept into the entry.
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): mine first"
  local first; first="$(git rev-parse HEAD)"
  fixture_touch_crate libdd-beta "feat(beta): NOT MINE"
  fixture_touch_crate libdd-alpha "fix(alpha): mine second"
  local commits
  commits="$(fixture_commits_json "${first}^..HEAD" -- libdd-alpha)"

  run_changelogs "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)"
  assert_success
  local out; out="$(changelog libdd-alpha)"
  assert_contains "$out" "Mine first"
  assert_contains "$out" "Mine second"
  assert_not_contains "$out" "NOT MINE"
}

@test "prepends the new entry and leaves earlier releases intact" {
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): add a thing"
  local commits; commits="$(fixture_commits_json HEAD~1..HEAD -- libdd-alpha)"

  run_changelogs "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)"
  assert_success
  local out; out="$(changelog libdd-alpha)"
  assert_contains "$out" "an older release"
  # Newest first: the new section must come before the one it was seeded with.
  local new_at old_at
  new_at="$(grep -n '\[1.1.0\]' <<< "$out" | head -1 | cut -d: -f1)"
  old_at="$(grep -n '\[1.0.0\]' <<< "$out" | head -1 | cut -d: -f1)"
  [ "$new_at" -lt "$old_at" ] || fail_with "new section is not above the old one" "$out"
}

@test "commits each generated CHANGELOG" {
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): add a thing"
  local commits; commits="$(fixture_commits_json HEAD~1..HEAD -- libdd-alpha)"
  local before; before="$(git rev-parse HEAD)"

  run_changelogs "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)"
  assert_success
  assert_eq "$(git rev-list --count "${before}..HEAD")" "1"
  assert_eq "$(git log -1 --format=%s)" "chore(release): update CHANGELOG.md for libdd-alpha"
  # Nothing left uncommitted.
  assert_eq "$(git status --porcelain -- libdd-alpha)" ""
}

# --- initial releases -------------------------------------------------------

@test "creates a minimal CHANGELOG for an initial release that has none" {
  run_changelogs "$(row libdd-delta 0.1.0 "" libdd-delta-v0.1.0 '[]' '[]' true)"
  assert_success
  assert_eq "$(changelog libdd-delta)" \
    "$(printf '# Changelog\n\n\n## 0.1.0 - %s\n\nInitial release.' "$RELEASE_DATE")"
  assert_contains "$(release_commits)" "chore(release): update CHANGELOG.md for libdd-delta"
}

@test "leaves an initial release's existing CHANGELOG untouched" {
  printf '# Changelog\n\nhand written\n' > "${FIXTURE_REPO}/libdd-delta/CHANGELOG.md"
  fixture_commit_all "chore: hand-written changelog"
  local before; before="$(git rev-parse HEAD)"

  run_changelogs "$(row libdd-delta 0.1.0 "" libdd-delta-v0.1.0 '[]' '[]' true)"
  assert_success
  assert_eq "$(changelog libdd-delta)" "$(printf '# Changelog\n\nhand written')"
  assert_eq "$(git rev-list --count "${before}..HEAD")" "0"
  assert_contains "$output" "Using existing CHANGELOG.md"
}

# --- dependency-only releases -----------------------------------------------

@test "writes a minimal entry for a crate released only for a dependency major bump" {
  seed_changelog libdd-beta 0.9.0
  run_changelogs "$(row libdd-beta 1.0.0 libdd-beta-v0.9.0 libdd-beta-v1.0.0 '[]' "$MB" false)"
  assert_success
  local out; out="$(changelog libdd-beta)"
  assert_contains "$out" '- Bump `libdd-core` to a new major version (`^1.0` → `^2.0`)'
  assert_contains "$out" "### Changed"
  # Same header shape git-cliff would have produced.
  assert_contains "$out" "## [1.0.0](https://github.com/datadog/libdatadog/compare/libdd-beta-v0.9.0..libdd-beta-v1.0.0) - ${RELEASE_DATE}"
  assert_contains "$out" "an older release"
}

@test "creates the file when a dependency-only release has no CHANGELOG yet" {
  run_changelogs "$(row libdd-beta 1.0.0 libdd-beta-v0.9.0 libdd-beta-v1.0.0 '[]' "$MB" false)"
  assert_success
  local out; out="$(changelog libdd-beta)"
  assert_contains "$out" "# Changelog"
  assert_contains "$out" '- Bump `libdd-core` to a new major version'
}

@test "omits the compare link when there is no previous tag" {
  run_changelogs "$(row libdd-beta 1.0.0 "" libdd-beta-v1.0.0 '[]' "$MB" false)"
  assert_success
  local out; out="$(changelog libdd-beta)"
  assert_contains "$out" "## [1.0.0] - ${RELEASE_DATE}"
  assert_not_contains "$out" "compare/"
}

@test "--remote-url changes the generated compare link" {
  run_changelogs --remote-url "https://example.invalid/org/repo" \
    "$(row libdd-beta 1.0.0 libdd-beta-v0.9.0 libdd-beta-v1.0.0 '[]' "$MB" false)"
  assert_success
  assert_contains "$(changelog libdd-beta)" "https://example.invalid/org/repo/compare/"
}

# --- nothing to say ---------------------------------------------------------

@test "writes nothing for a crate with no commits and no dependency bumps" {
  local before; before="$(git rev-parse HEAD)"
  run_changelogs "$(row libdd-beta 0.6.0 libdd-beta-v0.5.0 libdd-beta-v0.6.0 '[]' '[]' false)"
  assert_success
  [ ! -f "${FIXTURE_REPO}/libdd-beta/CHANGELOG.md" ] \
    || fail_with "a CHANGELOG was created for a crate with nothing to report"
  assert_eq "$(git rev-list --count "${before}..HEAD")" "0"
  assert_contains "$output" "skipping CHANGELOG generation"
}

@test "treats a missing major_bumps field as no bumps" {
  # Rows that never went through the audit have no major_bumps key at all.
  local no_mb
  no_mb="$(row libdd-beta 0.6.0 libdd-beta-v0.5.0 libdd-beta-v0.6.0 '[]' '[]' false | jq -c 'del(.major_bumps)')"
  run_changelogs "$no_mb"
  assert_success
  [ ! -f "${FIXTURE_REPO}/libdd-beta/CHANGELOG.md" ] || fail_with "unexpected CHANGELOG"
}

@test "does nothing for an empty release set" {
  local before; before="$(git rev-parse HEAD)"
  run_changelogs
  assert_success
  assert_eq "$(git rev-list --count "${before}..HEAD")" "0"
}

# --- argument handling ------------------------------------------------------

@test "fails when the jq that streams the rows fails" {
  # Regression, and the worst of the three: the caller's guard after this step is
  # `git diff --quiet "$EPHEMERAL_BRANCH"`, which still sees the version-bump commits
  # from the earlier step. A run that generated no CHANGELOG at all therefore looked
  # exactly like a healthy one, and the proposal shipped without changelogs.
  seed_changelog libdd-alpha 1.0.0
  fixture_touch_crate libdd-alpha "feat(alpha): add a thing"
  local commits before
  commits="$(fixture_commits_json HEAD~1..HEAD -- libdd-alpha)"
  before="$(git rev-parse HEAD)"
  printf '%s\n' "$(row libdd-alpha 1.1.0 libdd-alpha-v1.0.0 libdd-alpha-v1.1.0 "$commits" '[]' false)" \
    | jq -s '.' > "${TEST_TMP}/api-changes.json"

  use_failing_stream_jq
  run_tool release-generate-changelogs \
    --api-changes "${TEST_TMP}/api-changes.json"
  assert_failure 5
  assert_stderr_contains "simulated failure on the streaming read"
  assert_eq "$(git rev-list --count "${before}..HEAD")" "0"
}

@test "rejects a missing, non-file or non-array input" {
  run_tool release-generate-changelogs
  assert_failure
  assert_stderr_mentions "--api-changes"

  run_tool release-generate-changelogs --api-changes "${TEST_TMP}/nope.json"
  assert_failure
  assert_stderr_mentions "nope.json"

  echo '{"not":"an array"}' > "${TEST_TMP}/bad.json"
  run_tool release-generate-changelogs --api-changes "${TEST_TMP}/bad.json"
  assert_failure
  assert_stderr_mentions "bad.json" "array"

  run_tool release-generate-changelogs --nope
  assert_failure
  assert_stderr_mentions "--nope"
}
