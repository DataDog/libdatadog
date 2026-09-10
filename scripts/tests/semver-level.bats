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

# run_semver_level_for CRATE — same baseline, for the crates that carry dependencies.
# The baseline is just a revision, so libdd-alpha's tag serves for any crate.
run_semver_level_for() {
  run_tool semver-level "$1" "refs/tags/libdd-alpha-v1.2.3" HEAD
}

# run_semver_level_since CRATE BASELINE — for the cases that need state established in
# the baseline itself, and so tag their own.
run_semver_level_since() {
  run_tool semver-level "$1" "$2" HEAD
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

# --- dependency requirement floors -----------------------------------------
#
# Neither cargo-semver-checks nor cargo-public-api reads a manifest, so raising the
# lowest version a dependency admits is invisible to both and used to come out patch.
# Both tools stay stubbed clean throughout this section: the level under test can only
# have come from the manifest pass.

@test "raising a dependency requirement floor is a minor, not a patch" {
  fixture_set_dep_req libdd-beta libdd-alpha "1.3"
  fixture_commit_all "chore: require libdd-alpha 1.3"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "Dependency requirement floor raised"
  assert_contains "$(jq -r '.details' <<< "$output")" "libdd-alpha"
}

@test "a floor raised only inside [workspace.dependencies] is still seen" {
  # The libdd-capabilities case from release proposal #2482: `http = "1"` became
  # `http = { workspace = true }` with the root manifest at "1.1". The crate's own
  # Cargo.toml diff shows no version, so only a resolved view catches the raise.
  fixture_use_workspace_dep libdd-beta libdd-alpha "1.3"
  fixture_commit_all "refactor: migrate libdd-alpha to workspace level"
  # Guard the premise: the crate manifest really has no version left to read.
  assert_not_contains "$(cat "${FIXTURE_REPO}/libdd-beta/Cargo.toml")" 'version = "1.2"'
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "Dependency requirement floor raised"
}

@test "widening a requirement is not a raise and stays a patch" {
  # `bytes = "1.11.1"` -> `"1.11"` lowers the floor: every consumer that resolved
  # before still resolves.
  fixture_set_dep_req libdd-beta libdd-alpha "1.0"
  fixture_commit_all "chore: accept older libdd-alpha"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "patch"
  assert_jq "$output" '.reason' "No public API changes detected"
}

@test "a dependency is identified by its alias, not just its package name" {
  # cargo metadata reports the package name in .name and the local alias in .rename, so a
  # crate aliasing two versions of one package emits an identical name, kind and target
  # for both. Keyed without the alias, the second is looked up against the first's
  # baseline requirement and an unchanged pair reads as a raised floor.
  #
  # That exact pair needs a registry source — cargo refuses two same-named path packages
  # even in separate workspaces — and this fixture is deliberately offline, so assert
  # instead that the alias reaches the row, the lookup key and the report. Dropping it
  # from the emitted row shifts every later field and the raise stops being detected.
  fixture_alias_dep libdd-beta libdd-alpha alpha_next
  fixture_commit_all "refactor: alias libdd-alpha"
  fixture_publish_tag "beta-aliased"
  fixture_set_dep_req libdd-beta alpha_next "1.3"
  fixture_commit_all "chore: require libdd-alpha 1.3"
  run_semver_level_since libdd-beta "refs/tags/beta-aliased"
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_contains "$(jq -r '.details' <<< "$output")" "libdd-alpha as alpha_next"
}

@test "a dev-dependency floor raise stays a patch" {
  # libdd-tinybytes in #2482: its whole diff was a [dev-dependencies] entry. A consumer
  # never resolves those, so it must not be promoted.
  fixture_set_dep_req libdd-alpha libdd-gamma "2.0.1"
  fixture_commit_all "chore: bump the dev-dependency"
  run_semver_level
  assert_success
  assert_jq "$output" '.level' "patch"
}

@test "a build-dependency floor raise is a minor" {
  # libdd-delta build-depends on libdd-beta and dev-depends on libdd-alpha: the build
  # kind counts, and the dev one alongside it must not confuse the match.
  fixture_set_dep_req libdd-delta libdd-beta "0.5.1"
  fixture_commit_all "chore: require libdd-beta 0.5.1"
  run_semver_level_for libdd-delta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_contains "$(jq -r '.details' <<< "$output")" "libdd-beta"
}

@test "a raised major floor is capped at minor here" {
  # Whether a dependent must itself go major is release-version-major-bumps.sh's call,
  # made with the dependency graph this script cannot see. Reporting major here would
  # pre-empt it.
  fixture_set_dep_req libdd-beta libdd-alpha "2.0"
  fixture_commit_all "chore: require libdd-alpha 2.0"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
}

@test "tightening an inclusive bound into an exclusive one is a raise" {
  # `>=1.2` and `>1.2` are different floors — the second stops admitting 1.2 itself — so
  # comparing the bare version either side of the operator would call them equal and pass
  # a narrowed requirement off as a patch.
  fixture_set_dep_req libdd-beta libdd-alpha ">=1.2"
  fixture_commit_all "chore: require libdd-alpha >=1.2"
  fixture_publish_tag "beta-bounded"
  fixture_set_dep_req libdd-beta libdd-alpha ">1.2"
  fixture_commit_all "chore: stop admitting libdd-alpha 1.2"
  run_semver_level_since libdd-beta "refs/tags/beta-bounded"
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "Dependency requirement floor raised"
}

@test "relaxing an exclusive bound into an inclusive one is not a raise" {
  fixture_set_dep_req libdd-beta libdd-alpha ">1.2"
  fixture_commit_all "chore: require libdd-alpha >1.2"
  fixture_publish_tag "beta-bounded"
  fixture_set_dep_req libdd-beta libdd-alpha ">=1.2"
  fixture_commit_all "chore: admit libdd-alpha 1.2 again"
  run_semver_level_since libdd-beta "refs/tags/beta-bounded"
  assert_success
  assert_jq "$output" '.level' "patch"
}

@test "a requirement with no lower bound yields no opinion" {
  # `*` admits everything, so nothing was raised — and an unparsed requirement must
  # never be guessed into a bump.
  fixture_set_dep_req libdd-beta libdd-alpha "*"
  fixture_commit_all "chore: accept any libdd-alpha"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "patch"
}

@test "an unchanged manifest reports nothing from the dependency pass" {
  fixture_touch_crate libdd-beta "feat: something internal"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "patch"
  assert_jq "$output" '.reason' "No public API changes detected"
}

@test "an API signal of the same level keeps its more specific reason" {
  # Added items and a raised floor are both minor; the API explanation describes the
  # change better than "a dependency floor moved".
  stub_public_api 0 "$(public_api_diff '(none)' '(none)' 'pub fn libdd_beta::added()')"
  fixture_set_dep_req libdd-beta libdd-alpha "1.3"
  fixture_commit_all "chore: require libdd-alpha 1.3"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "cargo-public-api detected new public API items"
}

@test "a major API signal outranks a raised floor" {
  stub_public_api 0 "$(public_api_diff 'pub fn libdd_beta::gone()' '(none)' '(none)')"
  fixture_set_dep_req libdd-beta libdd-alpha "1.3"
  fixture_commit_all "chore: require libdd-alpha 1.3"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "cargo-public-api detected removed public API items"
}

# --- cargo feature surface --------------------------------------------------
#
# Features are public API — downstream writes `features = ["x"]` against them — but
# cargo-semver-checks has no lint for an ADDED feature, and an added feature adds no
# rustdoc item unless it gates one, so it used to come out patch. Both tools stay
# stubbed clean here, which also stands in for the crate shape where
# cargo-semver-checks is skipped outright (no library target) and this pass is the
# only thing watching the feature table.

@test "adding a feature is a minor" {
  fixture_set_features libdd-beta 'extra = []'
  fixture_commit_all "feat: add an extra feature"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "Cargo feature added"
  assert_contains "$(jq -r '.details' <<< "$output")" "extra"
}

@test "removing a feature is a major" {
  fixture_set_features libdd-beta 'default = ["std"]' 'std = []' 'extra = []'
  fixture_commit_all "feat: declare features"
  fixture_publish_tag "beta-featured"
  fixture_set_features libdd-beta 'default = ["std"]' 'std = []'
  fixture_commit_all "chore!: drop the extra feature"
  run_semver_level_since libdd-beta "refs/tags/beta-featured"
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "Cargo feature removed or no longer enabled by default"
  assert_contains "$(jq -r '.details' <<< "$output")" "extra"
}

@test "dropping a feature from the default set is a major" {
  # The feature is still declared, so a name-set comparison alone would miss it — but
  # every consumer relying on default features loses whatever it enabled.
  fixture_set_features libdd-beta 'default = ["std"]' 'std = []'
  fixture_commit_all "feat: declare features"
  fixture_publish_tag "beta-featured"
  fixture_set_features libdd-beta 'default = []' 'std = []'
  fixture_commit_all "chore!: stop enabling std by default"
  run_semver_level_since libdd-beta "refs/tags/beta-featured"
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "Cargo feature removed or no longer enabled by default"
  assert_contains "$(jq -r '.details' <<< "$output")" "std"
}

@test "the implicit feature of a newly optional dependency counts as added" {
  # `optional = true` derives a feature of the dependency's name that appears in no
  # [features] table, so only a resolved view of the manifest sees it.
  fixture_set_dep_optional libdd-beta libdd-alpha
  fixture_commit_all "refactor: make libdd-alpha optional"
  assert_not_contains "$(cat "${FIXTURE_REPO}/libdd-beta/Cargo.toml")" "[features]"
  run_semver_level_for libdd-beta
  assert_success
  assert_jq "$output" '.level' "minor"
  assert_jq "$output" '.reason' "Cargo feature added"
  assert_contains "$(jq -r '.details' <<< "$output")" "libdd-alpha"
}

@test "a removed feature outranks added public items" {
  # Also pins the asymmetry between the two manifest passes: the dependency pass stops
  # once the level is minor because minor is its ceiling, but this one must keep going,
  # or a feature removal on a crate whose cargo-semver-checks pass was skipped (no
  # library target) would ship as a minor.
  stub_public_api 0 "$(public_api_diff '(none)' '(none)' 'pub fn libdd_beta::added()')"
  fixture_set_features libdd-beta 'extra = []'
  fixture_commit_all "feat: declare features"
  fixture_publish_tag "beta-featured"
  fixture_set_features libdd-beta
  fixture_commit_all "chore!: drop the extra feature"
  run_semver_level_since libdd-beta "refs/tags/beta-featured"
  assert_success
  assert_jq "$output" '.level' "major"
  assert_jq "$output" '.reason' "Cargo feature removed or no longer enabled by default"
}

@test "renaming a feature reports the removal, not just the addition" {
  # A rename is an addition and a removal at once; the breaking half must win.
  fixture_set_features libdd-beta 'old_name = []'
  fixture_commit_all "feat: declare features"
  fixture_publish_tag "beta-featured"
  fixture_set_features libdd-beta 'new_name = []'
  fixture_commit_all "chore!: rename the feature"
  run_semver_level_since libdd-beta "refs/tags/beta-featured"
  assert_success
  assert_jq "$output" '.level' "major"
  assert_contains "$(jq -r '.details' <<< "$output")" "old_name"
}

@test "emits a single JSON object with the fields the workflow reads" {
  # The workflow does `jq -r '.level'` on this; extra chatter on stdout breaks it.
  run_semver_level
  assert_success
  assert_valid_json "$output"
  assert_jq "$output" 'keys | join(",")' "details,level,name,reason"
}
