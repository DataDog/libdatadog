---
phase: 02-mock-data-pre-processor
plan: 01
subsystem: infra
tags: [bp-analyzer, bats, bash, ci, benchmark, fixtures, json]

requires:
  - phase: 01-auth-ci-scaffolding
    provides: bench-analysis.yml CI job with auth and Claude invocation

provides:
  - BP v1 fixture files (baseline.json, candidate.json) with 4 scenarios and 4 metrics each
  - preprocess.sh invoking bp-analyzer compare pairwise to produce benchmark-comparison.md
  - preprocess.bats smoke test suite (6 tests) for the pre-processor pipeline
  - bench-analysis.yml wired to run preprocess.sh before the Claude invocation

affects:
  - 03-claude-analysis (reads artifacts/benchmark-comparison.md as Claude input)

tech-stack:
  added: [bats (test framework), bp-analyzer (pre-installed in CI image)]
  patterns: [BP v1 fixture schema, linear-jitter value arrays for statistical coverage, CI script separation from YAML inline]

key-files:
  created:
    - .gitlab/bench-analysis/fixtures/baseline.json
    - .gitlab/bench-analysis/fixtures/candidate.json
    - .gitlab/bench-analysis/preprocess.sh
    - .gitlab/bench-analysis/preprocess.bats
  modified:
    - .gitlab/bench-analysis.yml

key-decisions:
  - "D-02 user override: two monolithic fixture files (not per-benchmark-group split) — user confirmed 2026-06-16"
  - "Noise scenario (obfuscation-sql): candidate offset +300ns on 100000ns base with matching jitter to produce overlapping distributions"
  - "preprocess.sh is a separate committed file (not inline heredoc) for local testability"

patterns-established:
  - "Pattern: BP v1 fixture schema — schema_version + benchmarks array, parameters + runs[#1], 4 metrics with uom and 12-value arrays"
  - "Pattern: Linear jitter (base + i*step for i in [-5..6]) for unambiguous statistical coverage"
  - "Pattern: bp-analyzer compare pairwise with git_branch-based selectors (main vs pr-branch)"

requirements-completed: [DATA-01, DATA-02]

duration: 4min
completed: 2026-06-16
---

# Phase 02 Plan 01: Mock Data Pre-processor Summary

**BP v1 fixture files (4 scenarios, 4 metrics, 12 values each) + bp-analyzer preprocess.sh wired into bench-analysis.yml to produce artifacts/benchmark-comparison.md**

## Performance

- **Duration:** ~4 min
- **Started:** 2026-06-16T12:37:49Z
- **Completed:** 2026-06-16T12:40:57Z
- **Tasks:** 3
- **Files modified:** 5

## Accomplishments

- Created two BP v1 fixture files covering regression (normalize-service, +20%), improvement (concentrator, -15%), noise (obfuscation-sql, +0.3% overlapping), and unchanged (normalize-name, 0%) scenarios
- Created preprocess.sh invoking `bp-analyzer compare pairwise` with git_branch-based selectors, non-empty output assertion, and bp-analyzer availability probe
- Added 6-test Bats smoke suite (4 local + 2 CI-only guarded by bp-analyzer availability) with TDD RED/GREEN cycle
- Wired preprocess.sh into bench-analysis.yml between auth setup and the Claude smoke test

## Task Commits

1. **Task 1: Failing smoke test (TDD RED)** - `cd1ce19f4` (test)
2. **Task 2: BP v1 fixtures + preprocess.sh (TDD GREEN)** - `6cd13300e` (feat)
3. **Task 3: Wire preprocess.sh into bench-analysis.yml** - `a8ae2b63f` (ci)

## Files Created/Modified

- `.gitlab/bench-analysis/fixtures/baseline.json` - BP v1 baseline corpus, 4 scenarios (main branch, sha aaaaaa...)
- `.gitlab/bench-analysis/fixtures/candidate.json` - BP v1 candidate corpus, 4 scenarios (pr-branch, sha bbbbbb...)
- `.gitlab/bench-analysis/preprocess.sh` - bp-analyzer compare pairwise invocation with non-empty output guard
- `.gitlab/bench-analysis/preprocess.bats` - 6-test Bats smoke suite (4 local, 2 CI-only)
- `.gitlab/bench-analysis.yml` - Added `bash .gitlab/bench-analysis/preprocess.sh` step before Claude invocation

## Decisions Made

- D-02 override honored: two monolithic files instead of per-benchmark-group split. All 4 scenarios in one baseline.json + one candidate.json.
- Noise scenario (obfuscation-sql) uses candidate base 100,300 ns vs baseline 100,000 ns with matching step 100ns — ranges [99800..100900] vs [99500..100600] — overlapping distributions designed to produce `same` or `unsure` from bp-analyzer bootstrap CI.
- `#1` only for all runs (12 values each) matching Open Q3 recommendation.
- `cpu_usage_percentage` omitted from fixtures (not in D-03's four locked metrics).

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered

None.

## User Setup Required

None - no external service configuration required. `bp-analyzer` is pre-installed in the CI image (`dd-octo-sts-ci-base:2025.06-1`).

## Next Phase Readiness

- Phase 3 (Claude analysis) can now read `artifacts/benchmark-comparison.md` as its input
- Pipeline tests in preprocess.bats will run automatically in CI once bp-analyzer is available
- If bp-analyzer rejects the fixture schema (Open Q2 re: cpu_usage_percentage, Open Q1 re: positional file args), the fallback path is documented in the research notes and preprocess.sh can be updated without changing fixtures

---
*Phase: 02-mock-data-pre-processor*
*Completed: 2026-06-16*

## Self-Check: PASSED

All files present and all commits verified.
