# Roadmap: LLM Benchmark Analysis Pipeline

## Overview

Four phases build the pipeline from CI scaffolding through mock data, Claude analysis, and finally GitHub reporting. Each phase delivers a self-contained, verifiable capability. Phases 1 and 3 must complete before the pipeline can run end-to-end; Phase 2 unblocks Phase 3 by supplying the diff input; Phase 4 closes the loop with PR comment delivery.

## Phases

**Phase Numbering:**

- Integer phases (1, 2, 3): Planned milestone work
- Decimal phases (2.1, 2.2): Urgent insertions (marked with INSERTED)

Decimal phases appear between their surrounding integers in numeric order.

- [x] **Phase 1: Auth & CI Scaffolding** - GitLab CI job with AI Gateway and GitHub auth wired up (completed 2026-06-15)
- [x] **Phase 2: Mock Data & Pre-processor** - Fixture files and jq diff script producing benchmark-diff.json (completed 2026-06-16)
- [ ] **Phase 3: Claude Analysis** - System prompt, invocation script, and suspect code pointer
- [ ] **Phase 4: Reporting & GitHub Integration** - CI artifact declaration and PR comment posting

## Phase Details

### Phase 1: Auth & CI Scaffolding

**Goal**: The CI job exists, authenticates with both the AI Gateway and GitHub, and can invoke Claude Code
**Mode:** mvp
**Depends on**: Nothing (first phase)
**Requirements**: CI-01, CI-02, CI-03, CI-04
**Success Criteria** (what must be TRUE):

  1. A GitLab CI job triggers on libdatadog PR branches and runs to completion
  2. `ANTHROPIC_AUTH_TOKEN` is populated via `authanywhere --audience rapid-ai-platform` with no static secrets
  3. `GH_TOKEN` is populated via `dd-octo-sts` with no static PATs
  4. `claude --bare -p` with `--allowedTools "Read,Write,Glob,Grep"` and `--permission-mode bypassPermissions` is invocable in the CI environment**Plans**: 1 plan
- [x] 01-01-PLAN.md — Walking Skeleton: bench-analysis CI job + dd-octo-sts PR policy + end-to-end auth/Claude smoke test

### Phase 2: Mock Data & Pre-processor

**Goal**: BP v1 fixture files and a `bp-analyzer compare pairwise` pre-processor produce `artifacts/benchmark-comparison.md` without running real benchmarks
**Mode:** mvp
**Depends on**: Phase 1
**Requirements**: DATA-01, DATA-02
**Success Criteria** (what must be TRUE):

  1. Mock BP v1 before/after JSON fixtures exist covering regression, noise-level change, improvement, and unchanged benchmarks
  2. Running the `bp-analyzer` pre-processor against the fixtures produces `artifacts/benchmark-comparison.md` with per-metric significance classification (supersedes original jq/benchmark-diff.json plan, D-04/D-05/D-12)
  3. The comparison markdown is non-empty and names every benchmark scenario

**Plans**: 1 plan

- [x] 02-01-PLAN.md — BP v1 fixtures + bp-analyzer pre-processor producing benchmark-comparison.md, wired into bench-analysis.yml

### Phase 3: Claude Analysis

**Goal**: Claude reads the benchmark diff and PR diff, then produces a structured Markdown report
**Mode:** mvp
**Depends on**: Phase 2
**Requirements**: ANALYSIS-01, ANALYSIS-02, ANALYSIS-03
**Success Criteria** (what must be TRUE):

  1. The system prompt file exists and instructs Claude to emit a global verdict (pass/warn/fail), list regressions/improvements with noise guard applied, and prohibit hallucinated causes
  2. Running the invocation script produces a non-empty `artifacts/benchmark-report.md` (the script fails the CI job if the file is absent or empty)
  3. The report references specific files or functions from the PR diff when benchmarks overlap with changed code

**Plans**: 1 plan

- [ ] 03-01-PLAN.md — analyze-prompt.md system prompt + analyze.sh (PR diff context, non-empty assertion) + analyze.bats, wired into bench-analysis.yml replacing the smoke test

### Phase 4: Reporting & GitHub Integration

**Goal**: The report is saved as a CI artifact and posted (or updated) as a GitHub PR comment
**Mode:** mvp
**Depends on**: Phase 3
**Requirements**: REPORT-01, REPORT-02, REPORT-03
**Success Criteria** (what must be TRUE):

  1. `artifacts/benchmark-report.md` is declared as a GitLab CI artifact retained for at least 30 days
  2. The CI job posts the report as a GitHub PR comment; re-running the job updates the existing comment rather than creating a duplicate
  3. A dd-octo-sts policy file in `.github/chainguard/` grants `pull_requests: write` for PR branches (not restricted to main/release)

**Plans**: 1 plan

- [ ] 04-01-PLAN.md — report.sh posts/updates the GitHub PR comment (gh api + HTML marker dedup, no-PR guard) + report.bats, wired into bench-analysis.yml after analyze.sh

## Progress

**Execution Order:**
Phases execute in numeric order: 1 → 2 → 3 → 4

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| 1. Auth & CI Scaffolding | 1/1 | Complete   | 2026-06-15 |
| 2. Mock Data & Pre-processor | 1/1 | Complete   | 2026-06-16 |
| 3. Claude Analysis | 0/1 | Not started | - |
| 4. Reporting & GitHub Integration | 0/1 | Not started | - |
