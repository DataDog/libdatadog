---
phase: 02-mock-data-pre-processor
fixed_at: 2026-06-16T00:00:00Z
review_path: .planning/phases/02-mock-data-pre-processor/02-REVIEW.md
iteration: 1
findings_in_scope: 8
fixed: 7
skipped: 1
status: partial
---

# Phase 02: Code Review Fix Report

**Fixed at:** 2026-06-16T00:00:00Z
**Source review:** .planning/phases/02-mock-data-pre-processor/02-REVIEW.md
**Iteration:** 1

**Summary:**
- Findings in scope: 8 (3 Critical + 5 Warning)
- Fixed: 7
- Skipped: 1

## Fixed Issues

### CR-01: `preprocess.sh` hardcodes `pr-branch`

**Files modified:** `.gitlab/bench-analysis/preprocess.sh`
**Commit:** c36c9524c
**Applied fix:** Replaced hardcoded `"pr-branch"` and `"main"` strings with `$CANDIDATE_BRANCH` and `$BASELINE_BRANCH` env vars (defaulting to `${CI_COMMIT_REF_NAME:-pr-branch}` and `main` respectively). Also parameterized the fixture JSON paths via `$BASELINE_JSON` / `$CANDIDATE_JSON`.

---

### CR-02: `GH_TOKEN` acquisition failure silently swallowed

**Files modified:** `.gitlab/bench-analysis.yml`
**Commit:** 1081fe9e6
**Applied fix:** Removed `|| true` from the `dd-octo-sts` invocation so a token acquisition failure fails the job immediately.

---

### CR-03: `curl | bash` without `--fail`

**Files modified:** `.gitlab/bench-analysis.yml`
**Commit:** a3eac40d1
**Applied fix:** Added `--fail` flag to the `curl` command that fetches the nvm install script.

---

### WR-02: `ANTHROPIC_AUTH_TOKEN` extraction has no format validation

**Files modified:** `.gitlab/bench-analysis.yml`
**Commit:** d8ac09952
**Applied fix:** Added a prefix check after calling `authanywhere`; exits with a clear error if the output does not start with `Authorization: Bearer `.

---

### WR-03: bats test 6 reads stale `artifacts/` without teardown

**Files modified:** `.gitlab/bench-analysis/preprocess.bats`
**Commit:** b724115aa
**Applied fix:** Added a `setup()` function that removes `$COMPARISON_OUT` before each test, preventing stale artifact reuse.

---

### WR-04: Tests use repo-root-relative paths without enforcing CWD

**Files modified:** `.gitlab/bench-analysis/preprocess.bats`
**Commit:** 4ba677af6
**Applied fix:** Replaced static relative path strings with `BATS_TEST_DIRNAME`-derived absolute paths so the suite works regardless of the directory it is invoked from.

---

### WR-05: Fixtures share identical `ci_job_id`, `ci_pipeline_id`, `ci_job_date`

**Files modified:** `.gitlab/bench-analysis/fixtures/candidate.json`
**Commit:** 1d11462cf
**Applied fix:** Updated candidate fixture to use `ci_job_id: "100000002"`, `ci_pipeline_id: "200000002"`, `ci_job_date: "1718002000"` — distinct from the baseline values.

---

## Skipped Issues

### WR-01: `authanywhere` fetched from `LATEST` — unpinned binary in CI

**File:** `.gitlab/bench-analysis.yml:11`
**Reason:** skipped: fix requires a specific pinned version number for the `authanywhere` binary. The REVIEW.md suggestion uses `1.2.3` as a placeholder. Pinning to a placeholder would be worse than the current state. The correct version must be determined by the developer and pinned deliberately.
**Original issue:** `authanywhere` is downloaded from the `LATEST` URL making builds non-reproducible and vulnerable to silent breakage on format changes.

---

_Fixed: 2026-06-16T00:00:00Z_
_Fixer: Claude (gsd-code-fixer)_
_Iteration: 1_
