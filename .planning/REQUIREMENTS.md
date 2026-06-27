# Requirements — LLM Benchmark Analysis Pipeline

**Project:** Prophylactic Benchmarking — LLM Analysis  
**Defined:** 2026-06-15  
**Core Value:** Contributors get benchmark impact feedback on their libdatadog PR before merge

---

## v1 Requirements

### CI-01: GitLab CI job definition

- [x] **CI-01**: A GitLab CI job exists in `.gitlab-ci.yml` (or an included file) that runs the benchmark analysis pipeline on libdatadog PRs

### CI-02: AI Gateway auth

- [x] **CI-02**: The CI job authenticates with the Datadog AI Gateway via `authanywhere --audience rapid-ai-platform`, storing the bearer token as `ANTHROPIC_AUTH_TOKEN`

### CI-03: GitHub auth

- [x] **CI-03**: The CI job obtains a short-lived GitHub token via `dd-octo-sts` and exports it as `GH_TOKEN`; no static PATs are used

### CI-04: Claude Code CLI invocation

- [x] **CI-04**: The CI job invokes Claude Code CLI with `claude --bare -p` using `--allowedTools "Read,Write,Glob,Grep"` and `--permission-mode bypassPermissions`

### DATA-01: Mock Criterion fixtures

- [ ] **DATA-01**: Mock Criterion benchmark fixtures exist as before/after JSON files covering at least: one critical regression, one minor regression (within noise), one improvement, and several unchanged benchmarks

### DATA-02: Benchmark pre-processor

- [ ] **DATA-02**: A `jq` script processes the before/after fixture files and produces `benchmark-diff.json` containing per-benchmark delta%, change classification (Regressed/Improved/NoChange), and Criterion confidence interval bounds

### ANALYSIS-01: System prompt

- [ ] **ANALYSIS-01**: A system prompt file (`.gitlab/bench-analysis-prompt.md` or similar) instructs Claude to produce a global verdict (pass/warn/fail), list regressions and improvements, apply the noise guard using CI bounds, and explicitly prohibits hallucinating causes not visible in the diff or benchmark name

### ANALYSIS-02: Claude invocation script

- [ ] **ANALYSIS-02**: A shell script invokes Claude with the system prompt and benchmark diff, produces `artifacts/benchmark-report.md`, and asserts the output file is non-empty (fails the job if Claude produced nothing)

### ANALYSIS-03: Suspect code change pointer

- [ ] **ANALYSIS-03**: The PR diff (from `git diff main...HEAD`) is included in Claude's context so it can identify files/functions that overlap with regressing benchmarks

### REPORT-01: CI artifact

- [ ] **REPORT-01**: `artifacts/benchmark-report.md` is declared as a GitLab CI artifact and retained for at least 30 days

### REPORT-02: GitHub PR comment

- [ ] **REPORT-02**: The CI job posts the report as a GitHub PR comment using `gh pr comment`; if a benchmark comment already exists on the PR it is updated in place (no comment proliferation)

### REPORT-03: dd-octo-sts policy for PR branches

- [ ] **REPORT-03**: A Chainguard/dd-octo-sts policy file exists in `.github/chainguard/` granting `pull_requests: write` for PR branches (not restricted to `main`/`release` only)

---

## v2 Requirements

- **Label or manual trigger**: Trigger the pipeline via a GitHub label (e.g. `benchmark`) or manual workflow dispatch rather than on every push — depends on Augusto's triggering workstream
- **Mock dd-trace-py fixtures**: Before/after in pytest-benchmark JSON format — blocked on format clarification from the triggering workstream
- **Configurable regression threshold**: Env var to tune the pass/warn/fail cutoff (hardcoded for v1)
- **Real Criterion benchmark run**: Actually run `cargo bench` in CI against both `main` and PR branch — currently relies on provided artifacts
- **dd-trace-py real artifact integration**: Consume real benchmark artifacts from dd-trace-py CI once triggering workstream is complete

---

## Out of Scope

- Triggering actual benchmark runs in dd-trace-py (Augusto's workstream)
- Continuous benchmarking from `main` branch
- Automated performance improvement loop
- Flame graph integration
- Trend-over-time visualization
- Automated PR blocking based on benchmark results (too risky without dedicated benchmark runners)

---

## Traceability

| Requirement | Phase | Status |
|-------------|-------|--------|
| CI-01 | Phase 1 | Complete |
| CI-02 | Phase 1 | Complete |
| CI-03 | Phase 1 | Complete |
| CI-04 | Phase 1 | Complete |
| DATA-01 | Phase 2 | Pending |
| DATA-02 | Phase 2 | Pending |
| ANALYSIS-01 | Phase 3 | Pending |
| ANALYSIS-02 | Phase 3 | Pending |
| ANALYSIS-03 | Phase 3 | Pending |
| REPORT-01 | Phase 4 | Pending |
| REPORT-02 | Phase 4 | Pending |
| REPORT-03 | Phase 4 | Pending |
