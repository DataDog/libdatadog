#!/usr/bin/env bash
set -euo pipefail

BP_ANALYZER="${BP_ANALYZER:-$(command -v bp-analyzer 2>/dev/null || echo /opt/dogbrew/bin/bp-analyzer)}"
[ -x "$BP_ANALYZER" ] || { echo "ERROR: bp-analyzer not found" >&2; exit 1; }

BASELINE_JSON="${BASELINE_JSON:-.gitlab/bench-analysis/fixtures/baseline.json}"
CANDIDATE_JSON="${CANDIDATE_JSON:-.gitlab/bench-analysis/fixtures/candidate.json}"

mkdir -p artifacts

"$BP_ANALYZER" compare pairwise \
  --baseline '{"baseline_or_candidate":"baseline"}' \
  --candidate '{"baseline_or_candidate":"candidate"}' \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  "${BASELINE_JSON}" "${CANDIDATE_JSON}"

if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi

echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
