---
phase: 02
slug: mock-data-pre-processor
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-06-16
---

# Phase 02 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Bash script + file assertions (bats optional) |
| **Config file** | none |
| **Quick run command** | `ls .gitlab/bench-analysis/fixtures/baseline.json .gitlab/bench-analysis/fixtures/candidate.json && python3 -c "import json; json.load(open('.gitlab/bench-analysis/fixtures/baseline.json'))"` |
| **Full suite command** | `bash .gitlab/bench-analysis/preprocess.sh && test -s artifacts/benchmark-comparison.md` |
| **Estimated runtime** | ~5 seconds (local, no bp-analyzer); ~30 seconds (CI with bp-analyzer) |

---

## Sampling Rate

- **After every task commit:** Run quick run command (JSON validity check)
- **After every plan wave:** Run full suite (requires bp-analyzer in CI)
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 02-01-01 | 01 | 1 | DATA-01 | — | N/A | structural | `command -v bats >/dev/null 2>&1 && bats .gitlab/bench-analysis/preprocess.bats 2>&1 \| grep -qE 'not ok\|No such file' && echo RED-OK \|\| echo "SKIP: bats not installed"` | ❌ W0 | ⬜ pending |
| 02-01-02 | 01 | 1 | DATA-01, DATA-02 | — | N/A | smoke | `ls .gitlab/bench-analysis/fixtures/baseline.json .gitlab/bench-analysis/fixtures/candidate.json && python3 -c "import json; d=json.load(open('.gitlab/bench-analysis/fixtures/baseline.json')); assert d['schema_version']=='v1'; assert len(d['benchmarks'])>=4"` | ❌ W0 | ⬜ pending |
| 02-01-03 | 01 | 1 | DATA-02 | — | N/A | integration | `cat .gitlab/bench-analysis.yml \| grep -q 'preprocess.sh' && echo "CI-wired"` | ✅ | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `.gitlab/bench-analysis/fixtures/baseline.json` — BP v1 fixture file (main deliverable)
- [ ] `.gitlab/bench-analysis/fixtures/candidate.json` — BP v1 fixture file (main deliverable)
- [ ] `.gitlab/bench-analysis/preprocess.sh` — bp-analyzer invocation script (main deliverable)
- [ ] `.gitlab/bench-analysis/preprocess.bats` — bats smoke test (optional; guard for bats availability)

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| bp-analyzer produces `worse`/`better`/`same` verdicts for regression/improvement/noise scenarios | DATA-01 | Requires bp-analyzer binary (CI-only) | Run `bash .gitlab/bench-analysis/preprocess.sh` in CI and inspect `artifacts/benchmark-comparison.md` for 🟥/🟩 emoji per scenario |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 30s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
