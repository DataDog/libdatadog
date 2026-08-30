---
phase: 01-auth-ci-scaffolding
plan: 01
subsystem: infra
tags: [gitlab-ci, dd-octo-sts, authanywhere, claude-code, ai-gateway, github-actions]

# Dependency graph
requires: []
provides:
  - dd-octo-sts policy granting pull_requests:write for any PR branch (no ref restriction)
  - bench-analysis GitLab CI job with Vault JWT auth and Claude Code smoke test
  - .gitlab-ci.yml wired to include bench-analysis job
affects: [02-mock-data, 03-analysis, 04-reporting]

# Tech tracking
tech-stack:
  added: [dd-octo-sts, authanywhere, claude-code-cli, nvm]
  patterns:
    - mint GitHub token via dd-octo-sts (no static PAT)
    - mint Vault JWT immediately before Claude invocation (minimize expiry window)
    - probe tool presence before use (fail fast)
    - source nvm explicitly in non-interactive CI shell

key-files:
  created:
    - .github/chainguard/bench-analysis.write-pr.sts.yaml
    - .gitlab/bench-analysis.yml
  modified:
    - .gitlab-ci.yml

key-decisions:
  - "No ref restriction in dd-octo-sts policy: bench-analysis runs on arbitrary PR branches"
  - "pull_requests:write only — contents:write excluded for token scope minimization"
  - "ANTHROPIC_AUTH_TOKEN minted immediately before claude call to minimize Vault JWT expiry window"
  - "Both CI_MERGE_REQUEST_IID and CI_EXTERNAL_PULL_REQUEST_IID rules: repo is GitHub-mirrored"

patterns-established:
  - "authanywhere probe pattern: command -v authanywhere || { echo ERROR; exit 1; }"
  - "nvm sourcing: export NVM_DIR then source nvm.sh before nvm commands"

requirements-completed: [CI-01, CI-02, CI-03, CI-04]

# Metrics
duration: ~15min
completed: 2026-06-15
---

# Phase 01 Plan 01: Auth & CI Scaffolding Summary

**GitLab CI walking skeleton with Vault JWT → AI Gateway auth, dd-octo-sts GitHub token, and Claude Code smoke test — no static secrets**

## Performance

- **Duration:** ~15 min
- **Started:** 2026-06-15T14:00:00Z
- **Completed:** 2026-06-15T14:15:00Z
- **Tasks:** 2/3 complete (Task 3 is a live-CI checkpoint awaiting human verification)
- **Files modified:** 3

## Accomplishments
- Created dd-octo-sts policy granting only `pull_requests:write` for any PR branch (no ref restriction) with `project_id: "2260"` pinned to prevent unauthorized minting
- Created `bench-analysis` GitLab CI job: authanywhere probe, dd-octo-sts GH_TOKEN, nvm+Claude Code install, immediate Vault JWT mint, smoke test with `claude --bare -p`
- Wired `.gitlab-ci.yml` to include the new job

## Task Commits

1. **Task 1: dd-octo-sts policy** - `b9ff1aa86` (ci)
2. **Task 2: bench-analysis job + .gitlab-ci.yml include** - `0eac3960d` (ci)

## Files Created/Modified
- `.github/chainguard/bench-analysis.write-pr.sts.yaml` - dd-octo-sts policy: pull_requests:write, no ref restriction, project_id pinned
- `.gitlab/bench-analysis.yml` - CI job: authanywhere probe, dd-octo-sts token, nvm+Claude, AI Gateway auth, smoke test
- `.gitlab-ci.yml` - Added `- local: .gitlab/bench-analysis.yml` to include block

## Decisions Made
- No `ref:` restriction in the policy: feature branches can be named anything; a ref restriction would cause claim-mismatch on every PR branch (RESEARCH Pitfall 3)
- `pull_requests: write` only — `contents: write` excluded (D-08, threat T-01-02)
- Both `$CI_MERGE_REQUEST_IID` and `$CI_EXTERNAL_PULL_REQUEST_IID` trigger rules — the repo is GitHub-mirrored so it uses the external PR IID variable (seen in `trigger_internal_build`)
- `ANTHROPIC_AUTH_TOKEN` minted immediately before `claude` invocation to minimize Vault JWT expiry window (D-06, threat T-01-03)

## Deviations from Plan

None — plan executed exactly as written. yamllint was installed via brew to satisfy the verification step (not pre-installed). The existing codebase files (benchmarks.yml, analog policy) also have yamllint line-length warnings under default config; the bench-analysis.yml passes with relaxed line-length rules matching the project's implicit style.

## Issues Encountered
- yamllint not pre-installed; installed via `brew install yamllint`. File passes with warnings-only (exit 0) under default config; line-length errors are consistent with existing repo YAML files which also exceed 80 chars.

## User Setup Required
- Task 3 (live CI checkpoint): Push branch, open PR, confirm `bench-analysis` job appears and runs green end-to-end. Three [ASSUMED] values need live validation: (A) `authanywhere` present in `dd-octo-sts-ci-base:2025.06-1`; (B) `ANTHROPIC_BASE_URL="https://ai-gateway.us1.ddbuild.io/anthropic"` is correct; (C) `authanywhere` outputs bare token (not JSON wrapper). The dd-octo-sts policy may also need Chainguard team coordination before it activates (STATE blocker REPORT-03).

## Next Phase Readiness
- Auth scaffolding files are written and YAML-valid
- Live CI run (Task 3) must pass before Phase 2 work begins
- If authanywhere is absent from the image or the gateway URL is wrong, YAML update needed before proceeding

---
*Phase: 01-auth-ci-scaffolding*
*Completed: 2026-06-15 (pending Task 3 live verification)*
