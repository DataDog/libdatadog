---
phase: "03"
plan: "01"
subsystem: bench-analysis
tags: [ci, claude, benchmarks, analysis]
dependency_graph:
  requires: [02-01]
  provides: [benchmark-report.md]
  affects: [.gitlab/bench-analysis.yml]
tech_stack:
  added: []
  patterns: [claude-code-bare, system-prompt-file, pr_diff-injection]
key_files:
  created:
    - .gitlab/bench-analysis/analyze.bats
    - .gitlab/bench-analysis/analyze-prompt.md
    - .gitlab/bench-analysis/analyze.sh
  modified:
    - .gitlab/bench-analysis.yml
decisions:
  - PR diff injected as untrusted <pr_diff> block, capped at 50 KB via head -c 50000
  - System prompt enforces verdict tokens (pass/warn/fail) and Suspect code changes section
  - Smoke test removed in favour of real analyze.sh invocation
metrics:
  duration: ~5 min
  completed: 2026-06-17
---

# Phase 03 Plan 01: Claude Analysis Slice Summary

Delivers the Claude analysis slice: system prompt, shell driver, and CI wiring so the bench-analysis job produces `artifacts/benchmark-report.md` after preprocessing.

## Tasks Completed

| # | Task | Commit |
|---|------|--------|
| 1 | Create analyze.bats (RED) | a129234d8 |
| 2 | Create analyze-prompt.md and analyze.sh (GREEN) | b79d34545 |
| 3 | Wire analyze.sh into CI job, remove smoke test | e67fbb854 |

## What Was Built

- `analyze-prompt.md`: system prompt instructing Claude to classify benchmarks by bp-analyzer labels and identify overlapping file changes; pr_diff treated as untrusted
- `analyze.sh`: bash driver that fetches the PR diff (capped 50 KB), calls `claude --bare` with the system prompt, and asserts the report is non-empty
- `analyze.bats`: 4-test suite (3 static + 1 CI-only integration); static tests verify prompt tokens, pr_diff injection, and non-empty guard
- `bench-analysis.yml`: smoke-test line replaced with `bash .gitlab/bench-analysis/analyze.sh`

## Deviations from Plan

None — plan executed exactly as written.

## Self-Check: PASSED

- .gitlab/bench-analysis/analyze.bats: FOUND
- .gitlab/bench-analysis/analyze-prompt.md: FOUND
- .gitlab/bench-analysis/analyze.sh: FOUND
- .gitlab/bench-analysis.yml modified: FOUND
- Commits a129234d8, b79d34545, e67fbb854: FOUND
