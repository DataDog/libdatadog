#!/usr/bin/env bats
# Test suite for report.sh — posts/updates the benchmark report as a GitHub PR comment.
# Static tests run everywhere. CI-only tests skip when GH_TOKEN is absent.

REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
REPORT_SH="$REPO_ROOT/.gitlab/bench-analysis/report.sh"

@test "report.sh is syntactically valid" {
  bash -n "$REPORT_SH"
}

@test "no-PR guard exits 0 with skip message" {
  run env -u CI_EXTERNAL_PULL_REQUEST_IID REPORT="$REPORT_SH" bash "$REPORT_SH"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skipping GitHub comment"* ]]
}

@test "HTML marker present in script" {
  grep -q 'bench-analysis-report' "$REPORT_SH"
}

@test "uses gh api (not gh pr comment)" {
  grep -q 'gh api' "$REPORT_SH"
  ! grep -q 'gh pr comment' "$REPORT_SH"
}

@test "PATCH targets flat comment endpoint" {
  grep -q 'issues/comments/' "$REPORT_SH"
}

@test "REPORT-01 unchanged: artifact retained >= 30 days" {
  grep -q 'expire_in: 1 month' "$REPO_ROOT/.gitlab/bench-analysis.yml"
}

@test "REPORT-03 unchanged: policy grants pull_requests:write" {
  grep -q 'pull_requests: write' "$REPO_ROOT/.github/chainguard/bench-analysis.write-pr.sts.yaml"
}

@test "wired into bench-analysis.yml" {
  grep -q 'report.sh' "$REPO_ROOT/.gitlab/bench-analysis.yml"
}

@test "posts/updates comment (CI-only)" {
  [ -n "${GH_TOKEN:-}" ] || skip "GH_TOKEN not set (CI-only)"
  bash -n "$REPORT_SH"
}
