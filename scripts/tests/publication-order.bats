#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# publication-order.sh feeds the release-proposal-dispatch workflow the set of crates
# to release (the requested crates plus their workspace dependencies) in dependency
# order. Getting the set or the order wrong means publishing a crate before the
# dependency it needs, or silently dropping a crate from the release.

load helpers/common
load helpers/fixture

setup() {
  setup_common
  fixture_init
}

teardown() {
  teardown_common
}

# Dependency edges the publication order must respect (build-dependencies count,
# dev-dependencies do not — see helpers/fixture.bash for the fixture layout).
DEPS_SPEC='libdd-beta:libdd-alpha
libdd-gamma:libdd-alpha,libdd-beta
libdd-delta:libdd-beta
other-tool:libdd-alpha
libdd-private:libdd-alpha'

@test "orders every crate after its workspace dependencies" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple
  assert_success
  assert_topological_order "$output" "$DEPS_SPEC"
}

@test "a dev-dependency cycle does not trip the cycle detector" {
  # libdd-alpha dev-depends on libdd-gamma, which depends on libdd-alpha. If dev
  # edges leaked into the graph this would abort with "Circular dependency detected".
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple
  assert_success
  assert_not_contains "$output" "Circular dependency"
  assert_contains "$output" "libdd-alpha"
}

@test "excludes publish = false crates by default" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple
  assert_success
  assert_not_contains "$output" "libdd-private"
}

@test "--include-unpublishable and --all add publish = false crates back" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple --include-unpublishable
  assert_success
  assert_contains "$output" "libdd-private"

  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple --all
  assert_success
  assert_contains "$output" "libdd-private"
}

@test "json format reports name and version for each crate" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=json
  assert_success
  assert_valid_json "$output"
  assert_jq "$output" '[.[] | select(.name == "libdd-alpha")] | length' "1"
  assert_jq "$output" '.[] | select(.name == "libdd-gamma") | .version' "2.0.1"
  # The workflow pipes this straight into commits-since-release.sh, which reads
  # exactly these two fields.
  assert_jq "$output" '[.[] | keys] | flatten | unique | join(",")' "name,version"
}

@test "filtering by a crate includes its transitive dependencies and nothing else" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-gamma
  assert_success
  assert_eq "$(sort <<< "$output")" "$(printf 'libdd-alpha\nlibdd-beta\nlibdd-gamma')"
  assert_topological_order "$output" "$DEPS_SPEC"
}

@test "filtering by a leaf crate returns only that crate" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-alpha
  assert_success
  assert_eq "$output" "libdd-alpha"
}

@test "filtering by several crates unions their dependency closures" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-delta other-tool
  assert_success
  assert_eq "$(sort <<< "$output")" \
    "$(printf 'libdd-alpha\nlibdd-beta\nlibdd-delta\nother-tool')"
  assert_topological_order "$output" "$DEPS_SPEC"
}

@test "a dev-dependency is not pulled into the dependency closure" {
  # libdd-delta dev-depends on libdd-alpha and build-depends on libdd-beta.
  # libdd-alpha still appears, but only because libdd-beta depends on it.
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-delta
  assert_success
  assert_eq "$(sort <<< "$output")" "$(printf 'libdd-alpha\nlibdd-beta\nlibdd-delta')"
}

@test "requesting a repeated crate is idempotent" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-beta libdd-beta
  assert_success
  assert_eq "$(sort <<< "$output")" "$(printf 'libdd-alpha\nlibdd-beta')"
}

@test "output is deterministic across runs" {
  # The workflow branch name and the PR body are derived from this order; churn
  # between runs would make proposals non-reproducible.
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=json
  assert_success
  local first="$output"
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=json
  assert_success
  assert_eq "$output" "$first"
}

@test "rejects an unknown crate" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=json libdd-nope
  assert_failure 1
  assert_stderr_contains "Unknown crate 'libdd-nope'"
}

@test "rejects a publish = false crate as a target unless unpublishable crates are included" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple libdd-private
  assert_failure 1
  assert_stderr_contains "Unknown crate 'libdd-private'"

  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=simple --all libdd-private
  assert_success
  assert_contains "$output" "libdd-private"
}

@test "rejects an unknown format and an unknown option" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=yaml
  assert_failure 1
  assert_stderr_contains "Unknown format: yaml"

  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --nope
  assert_failure 1
  assert_stderr_contains "Unknown option: --nope"
}

@test "list format annotates unpublishable crates and shows dependencies" {
  run --separate-stderr "${SCRIPTS_DIR}/publication-order.sh" --format=list --all
  assert_success
  assert_contains "$output" "libdd-private (0.9.0) [unpublishable]"
  assert_contains "$output" "Dependencies: libdd-alpha (1.2.3)"
}
