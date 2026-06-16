#!/usr/bin/env bash
set -euo pipefail

command -v bp-analyzer >/dev/null || { echo "ERROR: bp-analyzer not found in PATH" >&2; exit 1; }

mkdir -p artifacts

bp-analyzer compare pairwise \
  --baseline '{"git_branch":"main"}' \
  --candidate '{"git_branch":"pr-branch"}' \
  --format=md \
  --outpath=artifacts/benchmark-comparison.md \
  .gitlab/bench-analysis/fixtures/baseline.json \
  .gitlab/bench-analysis/fixtures/candidate.json

if [ ! -s artifacts/benchmark-comparison.md ]; then
  echo "ERROR: benchmark-comparison.md is empty — bp-analyzer produced no output" >&2
  exit 1
fi

echo "benchmark-comparison.md generated ($(wc -l < artifacts/benchmark-comparison.md) lines)"
