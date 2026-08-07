#!/usr/bin/env bats

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Structural guards on .github/workflows/release-proposal-dispatch.yml.
#
# Most of that workflow's logic is inline bash and is only exercised by running a real
# release. What CANNOT be exercised any other way is its job-level gating: the `if:`
# expressions that decide whether a release proceeds, and the steps that reject an
# untrusted `main_start_ref`. Those are one careless edit away from silently letting a
# release through without the membership check, or running attacker-controlled code
# with the job's OIDC token.
#
# These tests are deliberately change-detectors — each one states WHY the invariant
# exists, so a deliberate change means updating the test and reading the reason first.
# They complement actionlint (lint.yml), which checks syntax but not intent.

load helpers/common

WORKFLOW=".github/workflows/release-proposal-dispatch.yml"
TEST_WORKFLOW=".github/workflows/release-proposal-test.yml"

setup() {
  setup_common
  WF="$(python3 "${TESTS_DIR}/helpers/workflow-to-json.py" "${REPO_ROOT}/${WORKFLOW}")"
  TEST_WF="$(python3 "${TESTS_DIR}/helpers/workflow-to-json.py" "${REPO_ROOT}/${TEST_WORKFLOW}")"
}

teardown() {
  teardown_common
}

# job_if JOB — the job's `if:` expression, with runs of whitespace collapsed to a
# single space so line wrapping in the YAML does not affect the assertions.
job_if() {
  jq -r --arg job "$1" '.jobs[$job].if // ""' <<< "$WF" \
    | tr -s '[:space:]' ' ' | sed -e 's/^ //' -e 's/ $//'
}

# step_run JOB STEP_NAME — the `run:` body of a named step.
step_run() {
  jq -r --arg job "$1" --arg name "$2" \
    '.jobs[$job].steps[] | select(.name == $name) | .run // ""' <<< "$WF"
}

# step_if JOB STEP_NAME — the `if:` of a named step.
step_if() {
  jq -r --arg job "$1" --arg name "$2" \
    '.jobs[$job].steps[] | select(.name == $name) | .if // ""' <<< "$WF"
}

# --- privilege gating -------------------------------------------------------

@test "the membership check is skipped only when bypassing standard checks" {
  assert_eq "$(job_if check-membership)" '${{ !inputs.bypass_standard_checks }}'
}

@test "cargo-release requires the ongoing-proposal guard to have succeeded" {
  # check-membership is skipped on bypass runs, so cargo-release accepts a skipped
  # membership. Without this second condition, a membership skipped because
  # check-proposal-ongoing FAILED would also satisfy the gate and release anyway.
  assert_contains "$(job_if cargo-release)" "needs.check-proposal-ongoing.result == 'success'"
}

@test "cargo-release requires validated inputs" {
  assert_contains "$(job_if cargo-release)" "needs.validate-inputs.result == 'success'"
}

@test "cargo-release accepts a skipped membership check only under bypass" {
  # The skipped-membership escape hatch must stay welded to the bypass input.
  local expr
  expr="$(job_if cargo-release)"
  assert_contains "$expr" "needs.check-membership.result == 'success'"
  assert_contains "$expr" "(inputs.bypass_standard_checks && needs.check-membership.result == 'skipped')"
}

@test "cargo-release depends on every job whose result it gates on" {
  # `needs.<job>.result` is empty for a job that is not in `needs`, which would make
  # each gate above silently unsatisfiable — or, with !cancelled(), simply wrong.
  local needs
  needs="$(jq -r '.jobs["cargo-release"].needs | sort | join(",")' <<< "$WF")"
  assert_eq "$needs" "check-membership,check-proposal-ongoing,validate-inputs"
}

@test "create-pr gates on cargo-release directly rather than on transitive status" {
  # A skipped check-membership propagates 'skipped' transitively through cargo-release,
  # so create-pr must name cargo-release's own result.
  assert_contains "$(job_if create-pr)" "needs.cargo-release.result == 'success'"
  assert_contains "$(job_if create-pr)" "needs.validate-inputs.result == 'success'"
  local needs
  needs="$(jq -r '.jobs["create-pr"].needs | sort | join(",")' <<< "$WF")"
  assert_eq "$needs" "cargo-release,validate-inputs"
}

@test "only the membership job may mint an org-read token" {
  # dd-octo-sts with the self.read.members policy must not spread to other jobs.
  local jobs
  jobs="$(jq -r '[.jobs | to_entries[]
                 | select(any(.value.steps[]?; (.with.policy? // "") == "self.read.members"))
                 | .key] | join(",")' <<< "$WF")"
  assert_eq "$jobs" "check-membership"
}

# --- untrusted main_start_ref ----------------------------------------------

@test "the checked-out tree is rejected if it configures cargo-release hooks" {
  # cargo-release's pre-release-hook / pre-release-replacements run arbitrary commands.
  # main_start_ref chooses the tree, so without this guard a crafted ref executes code
  # in a job that holds an OIDC minting capability.
  local body
  body="$(step_run cargo-release "Reject untrusted cargo-release configuration")"
  assert_contains "$body" "pre-release-hook"
  assert_contains "$body" "pre-release-replacements"
  assert_contains "$body" "exit 1"
}

@test "pull-request refs are rejected as main_start_ref" {
  # Anyone with a fork can push refs/pull/*.
  local body
  body="$(step_run cargo-release "Optionally checkout at a specific git ref")"
  assert_contains "$body" "refs/pull/*|pull/*)"
  assert_contains "$body" "refs/pull/* refs are not allowed"
}

@test "main_start_ref must resolve to a commit reachable from a trusted branch" {
  local body
  body="$(step_run cargo-release "Optionally checkout at a specific git ref")"
  assert_contains "$body" 'git merge-base --is-ancestor "$COMMIT" "origin/${{ env.MAIN_BRANCH }}"'
  assert_contains "$body" 'TRUSTED=false'
  assert_contains "$body" 'if [ "$TRUSTED" != "true" ]; then'
}

@test "the hotfix ref pattern is anchored" {
  # An unanchored pattern would let any ref containing a hotfix-looking substring take
  # the hotfix path, which skips the ephemeral-branch creation and the latest-tag guard.
  local pattern
  pattern="$(jq -r '.env.HOTFIX_REF_PATTERN' <<< "$WF")"
  assert_eq "$pattern" '^hotfix/[^/]+/[0-9]+\.x\.x$'
}

@test "hotfix releases are limited to a single crate" {
  # The hotfix path releases onto the hotfix branch itself; bundling several crates
  # there would publish unrelated crates from an old base.
  local body
  body="$(step_run validate-inputs "Normalize and validate crate list")"
  assert_contains "$body" "hotfix releases (main_start_ref=\$REF) accept only a single crate"
}

@test "only publishable crates can be requested" {
  local body
  body="$(step_run validate-inputs "Normalize and validate crate list")"
  assert_contains "$body" "unknown or unpublishable crate(s)"
  assert_contains "$body" 'select(.publish == null or (.publish | type == "array" and length > 0))'
}

@test "release tooling is snapshotted from the workflow revision, not from main_start_ref" {
  # main_start_ref may be an old or crafted tree; the scripts that run with the job's
  # privileges must come from the revision the workflow file itself was read from.
  local body
  body="$(step_run cargo-release "Snapshot scripts from workflow revision")"
  assert_contains "$body" 'WF_SHA="${{ github.sha }}"'
  assert_contains "$body" 'git archive "$WF_SHA" scripts'
  assert_contains "$body" "WORKFLOW_SCRIPTS_ROOT="

  # And every release script call must go through that snapshot.
  local calls
  calls="$(jq -r '[.jobs[].steps[]?.run // ""] | join("\n")' <<< "$WF" \
    | grep -oE '(\$\{WORKFLOW_SCRIPTS_ROOT\}|\./scripts)/[a-z-]+\.sh' | sort -u)"

  # Keep this assertion from passing vacuously if the invocation syntax changes in
  # a way the matcher above no longer recognises.
  local expected
  for expected in \
    commits-since-release.sh \
    publication-order.sh \
    release-generate-changelogs.sh \
    release-version-bumps.sh \
    release-version-major-bumps.sh; do
    assert_contains "$calls" "\${WORKFLOW_SCRIPTS_ROOT}/$expected"
  done

  while IFS= read -r call; do
    [[ -z "$call" ]] && continue
    case "$call" in
      '${WORKFLOW_SCRIPTS_ROOT}/'*) ;;
      ./scripts/exclude-from-green-ci.sh) ;;  # runs in validate-inputs, before any ref switch
      *) fail_with "release script invoked outside the pinned snapshot: $call" ;;
    esac
  done <<< "$calls"
}

# --- branch prefixes --------------------------------------------------------

@test "bypass runs use separate branch prefixes so they cannot collide with real releases" {
  assert_eq "$(jq -r '.env.RELEASE_BRANCH_PREFIX' <<< "$WF")" \
    "\${{ inputs.bypass_standard_checks && 'release-testing' || 'release' }}"
  assert_eq "$(jq -r '.env.PROPOSAL_BRANCH_PREFIX' <<< "$WF")" \
    "\${{ inputs.bypass_standard_checks && 'release-proposal-testing' || 'release-proposal' }}"
}

@test "release-proposal-test.yml triggers on both the real and the testing prefixes" {
  # The two workflows are coupled by branch name only. Renaming a prefix in the
  # dispatch workflow without updating the triggers here silently stops the
  # dd-trace-rs compatibility check from ever running on a proposal.
  assert_eq "$(jq -r '.on.push.branches | sort | join(",")' <<< "$TEST_WF")" \
    "release-proposal-testing/**,release-proposal/**"
  assert_eq "$(jq -r '.on.pull_request.branches | sort | join(",")' <<< "$TEST_WF")" \
    "release-testing/**,release/**"
}

@test "the ongoing-proposal guard looks for both proposal and ephemeral release branches" {
  local body
  body="$(step_run check-proposal-ongoing "Check if a release proposal is ongoing")"
  assert_contains "$body" 'origin/${{ env.PROPOSAL_BRANCH_PREFIX }}/*'
  assert_contains "$body" 'origin/${{ env.RELEASE_BRANCH_PREFIX }}/*/*'
  assert_contains "$body" "A release proposal is ongoing"
}

# --- pushing ----------------------------------------------------------------

@test "real runs push through the verified-commit action and bypass runs push plainly" {
  # Swapping these conditions would push unverified commits onto a real release branch.
  assert_eq "$(step_if cargo-release "Push commits (verified)")" \
    '${{ !inputs.bypass_standard_checks }}'
  assert_eq "$(step_if cargo-release "Push commits (plain, testing only)")" \
    '${{ inputs.bypass_standard_checks }}'

  local uses
  uses="$(jq -r '.jobs["cargo-release"].steps[] | select(.name == "Push commits (verified)") | .uses' <<< "$WF")"
  assert_contains "$uses" "DataDog/commit-headless"
}

@test "the verified push is given commits oldest-first" {
  # commit-headless replays commits in the order given; newest-first (plain `git log`)
  # would fail to apply or reorder history.
  local body
  body="$(step_run cargo-release "Generate CHANGELOGS")"
  assert_contains "$body" 'git log --reverse "$ORIGINAL_HEAD"..'
}

@test "both branch-creating jobs clean up their branches on failure" {
  # A failed run that leaves branches behind trips the ongoing-proposal guard and
  # blocks every subsequent release until someone deletes them by hand.
  for job in cargo-release create-pr; do
    local cleanup
    cleanup="$(jq -r --arg job "$job" \
      '.jobs[$job].steps[] | select(.name == "Cleanup on failure") | .if' <<< "$WF")"
    assert_contains "$cleanup" "failure()"
    local body
    body="$(step_run "$job" "Cleanup on failure")"
    assert_contains "$body" "git push origin --delete"
    # The hotfix base branch is a real long-lived branch, not an ephemeral one.
    assert_contains "$body" "Hotfix mode: not deleting ephemeral base branch"
  done
}

# --- downstream contracts ---------------------------------------------------

@test "the PR title keeps the prefix downstream tooling matches on" {
  local body
  body="$(step_run create-pr "Create a PR")"
  assert_contains "$body" 'PR_TITLE="chore(release): proposal for'
  # GitHub rejects titles over 256 chars; the workflow truncates well below that.
  assert_contains "$body" 'if [ "${#PR_TITLE}" -gt 100 ]; then'
}

@test "the proposal PR is opened as a draft with the release-proposal label" {
  local body
  body="$(step_run create-pr "Create a PR")"
  assert_contains "$body" '--label "release-proposal"'
  assert_contains "$body" "--draft"
  assert_contains "$body" '--base "${{ needs.cargo-release.outputs.ephemeral_branch }}"'
}

@test "a failed PR creation fails the run unless it is a bypass run" {
  # Otherwise the branches survive and block the next release.
  local body bypass_marker standard_marker bypass_body standard_body
  body="$(step_run create-pr "Create a PR")"
  bypass_marker='elif [ "$BYPASS_STANDARD_CHECKS" = "true" ]; then'
  standard_marker='# Standard run: PR creation must succeed.'
  assert_contains "$body" "$bypass_marker"
  assert_contains "$body" "$standard_marker"

  bypass_body="${body#*"$bypass_marker"}"
  bypass_body="${bypass_body%%"$standard_marker"*}"
  assert_not_contains "$bypass_body" "exit 1"

  standard_body="${body#*"$standard_marker"}"
  assert_contains "$standard_body" '::error::Failed to create the release proposal PR.'
  assert_contains "$standard_body" "exit 1"
}

# --- run safety -------------------------------------------------------------

@test "concurrent dispatches queue instead of cancelling each other" {
  # Cancelling a run mid-release leaves pushed branches and a half-applied proposal.
  assert_eq "$(jq -r '.concurrency.group' <<< "$WF")" "release-proposal-dispatch-group"
  assert_eq "$(jq -r '.concurrency["cancel-in-progress"]' <<< "$WF")" "false"
}

@test "the workflow is dispatch-only" {
  # It pushes branches and mints tokens; it must never become push- or PR-triggered.
  assert_eq "$(jq -r '.on | keys | join(",")' <<< "$WF")" "workflow_dispatch"
}
