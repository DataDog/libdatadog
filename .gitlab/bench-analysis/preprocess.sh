#!/usr/bin/env bash
set -euo pipefail

command -v bp-analyzer >/dev/null || { echo "ERROR: bp-analyzer not found in PATH" >&2; exit 1; }

BASELINE_BRANCH="${BASELINE_BRANCH:-main}"
CANDIDATE_BRANCH="${CANDIDATE_BRANCH:-${CI_COMMIT_REF_NAME:-pr-branch}}"
BASELINE_JSON="${BASELINE_JSON:-.gitlab/bench-analysis/fixtures/baseline.json}"
CANDIDATE_JSON="${CANDIDATE_JSON:-.gitlab/bench-analysis/fixtures/candidate.json}"

mkdir -p artifacts

bp-analyzer compare pairwise \
  --baseline "{\"git_branch\":\"${BASELINE_BRANCH}\"}" \
  --candidate "{\"git_branch\":\"${CANDIDATE_BRANCH}\"}" \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  "${BASELINE_JSON}" "${CANDIDATE_JSON}"

if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi

echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
