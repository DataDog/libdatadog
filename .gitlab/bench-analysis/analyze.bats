#!/usr/bin/env bats
# Test suite for the Claude analysis slice.
# Static tests (prompt-tokens, pr_diff-injection, non-empty-guard) run everywhere.
# Integration test (analyze.sh produces non-empty report) requires claude in PATH and CI fixtures.

REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
ANALYZE_SH="$REPO_ROOT/.gitlab/bench-analysis/analyze.sh"
PROMPT_FILE="$REPO_ROOT/.gitlab/bench-analysis/analyze-prompt.md"
REPORT_OUT="$REPO_ROOT/artifacts/benchmark-report.md"
COMPARISON_OUT="$REPO_ROOT/artifacts/benchmark-comparison.md"

setup() {
  rm -f "$REPORT_OUT"
}

@test "prompt file contains verdict tokens and Suspect code changes heading" {
  [ -f "$PROMPT_FILE" ]
  grep -v '^#' "$PROMPT_FILE" | grep -q 'pass'
  grep -v '^#' "$PROMPT_FILE" | grep -q 'warn'
  grep -v '^#' "$PROMPT_FILE" | grep -q 'fail'
  grep -q 'Suspect code changes' "$PROMPT_FILE"
}

@test "analyze.sh injects PR diff under pr_diff delimiter" {
  [ -f "$ANALYZE_SH" ]
  grep -q 'pr_diff' "$ANALYZE_SH"
}

@test "analyze.sh asserts non-empty output and references report path" {
  [ -f "$ANALYZE_SH" ]
  grep -q 'is empty' "$ANALYZE_SH"
  grep -q 'benchmark-report.md' "$ANALYZE_SH"
}

@test "analyze.sh produces non-empty artifacts/benchmark-report.md (CI-only)" {
  command -v claude >/dev/null || skip "claude not available (CI-only)"
  [ -s "$COMPARISON_OUT" ] || skip "benchmark-comparison.md missing — run preprocess.sh first"
  bash "$ANALYZE_SH"
  [ -s "$REPORT_OUT" ]
}
