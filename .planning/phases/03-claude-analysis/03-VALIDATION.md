---
phase: 3
slug: claude-analysis
status: draft
nyquist_compliant: true
wave_0_complete: false
created: 2026-06-17
---

# Phase 3 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | bats (Bash Automated Testing System) |
| **Config file** | none — tests use `#!/usr/bin/env bats` shebang |
| **Quick run command** | `bats .gitlab/bench-analysis/analyze.bats` |
| **Full suite command** | `bats .gitlab/bench-analysis/` |
| **Estimated runtime** | ~10 seconds |

---

## Sampling Rate

- **After every task commit:** Run `bats .gitlab/bench-analysis/analyze.bats`
- **After every plan wave:** Run `bats .gitlab/bench-analysis/`
- **Before `/gsd-verify-work`:** Full suite must be green
- **Max feedback latency:** 30 seconds

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 3-01-01 | 01 | 1 | ANALYSIS-01 | — | System prompt prohibits hallucinated causes | manual | inspect `analyze-prompt.md` | ❌ W0 | ⬜ pending |
| 3-01-02 | 01 | 1 | ANALYSIS-02 | — | Script produces non-empty report | unit | `bats .gitlab/bench-analysis/analyze.bats` | ❌ W0 | ⬜ pending |
| 3-01-03 | 01 | 2 | ANALYSIS-03 | — | Report references PR diff content | integration | dry-run with fixture data | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `.gitlab/bench-analysis/analyze.bats` — stubs for ANALYSIS-01, ANALYSIS-02, ANALYSIS-03
- [ ] Fixture: `artifacts/benchmark-comparison.md` — sample comparison data (produced by preprocess.sh)
- [ ] PR diff: extracted in-script by `analyze.sh` via `git diff origin/main...HEAD`

*Existing bats infrastructure from Phase 2 covers the test runner setup.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| Report quality and accuracy | ANALYSIS-01 | LLM output quality cannot be asserted deterministically | Inspect `artifacts/benchmark-report.md` for verdict, regression list, and PR diff references |
| System prompt prohibits hallucination | ANALYSIS-01 | Content review required | Verify `analyze-prompt.md` contains explicit instruction against hallucinated causes |

---

## Validation Sign-Off

- [x] All tasks have `<automated>` verify or Wave 0 dependencies
- [x] Sampling continuity: no 3 consecutive tasks without automated verify
- [x] Wave 0 covers all MISSING references
- [x] No watch-mode flags
- [x] Feedback latency < 30s
- [x] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
