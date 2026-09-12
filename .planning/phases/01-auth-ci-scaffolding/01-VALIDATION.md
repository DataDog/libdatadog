---
phase: 1
slug: auth-ci-scaffolding
status: draft
nyquist_compliant: false
wave_0_complete: false
created: 2026-06-15
---

# Phase 1 — Validation Strategy

> Per-phase validation contract for feedback sampling during execution.

---

## Test Infrastructure

| Property | Value |
|----------|-------|
| **Framework** | Shell/CI validation (no test framework — infra phase) |
| **Config file** | `.gitlab-ci.yml` |
| **Quick run command** | `gitlab-ci-lint .gitlab-ci.yml` (or `yamllint`) |
| **Full suite command** | Trigger the CI job on a test branch |
| **Estimated runtime** | ~5–15 minutes (CI job) |

---

## Sampling Rate

- **After every task commit:** Run `yamllint .gitlab-ci.yml` or `gitlab-ci-lint`
- **After every plan wave:** Trigger CI pipeline on PR branch and verify job completes
- **Before `/gsd-verify-work`:** Full CI pipeline must reach green
- **Max feedback latency:** 15 minutes

---

## Per-Task Verification Map

| Task ID | Plan | Wave | Requirement | Threat Ref | Secure Behavior | Test Type | Automated Command | File Exists | Status |
|---------|------|------|-------------|------------|-----------------|-----------|-------------------|-------------|--------|
| 1-01-01 | 01 | 1 | CI-01 | — | CI job YAML valid | lint | `yamllint .gitlab-ci.yml` | ❌ W0 | ⬜ pending |
| 1-01-02 | 01 | 1 | CI-02 | — | No static secrets in YAML | manual | inspect YAML for hardcoded tokens | ❌ W0 | ⬜ pending |
| 1-01-03 | 01 | 1 | CI-03 | — | dd-octo-sts policy file valid | lint | `yamllint` on policy file | ❌ W0 | ⬜ pending |
| 1-01-04 | 01 | 1 | CI-04 | — | claude invocable in CI | manual | check CI log for claude version output | ❌ W0 | ⬜ pending |

*Status: ⬜ pending · ✅ green · ❌ red · ⚠️ flaky*

---

## Wave 0 Requirements

- [ ] `.gitlab-ci.yml` — CI job scaffolding (or extend existing)
- [ ] `.github/chainguard/gitlab.github-access.prophylactic-bench.sts.yaml` — dd-octo-sts policy for PR branches

*CI infra phases cannot pre-stub tests before the job exists — Wave 0 creates the job and policy files.*

---

## Manual-Only Verifications

| Behavior | Requirement | Why Manual | Test Instructions |
|----------|-------------|------------|-------------------|
| ANTHROPIC_AUTH_TOKEN populated via authanywhere | CI-02 | Requires live CI run with Vault OIDC | Inspect CI log for `authanywhere --audience rapid-ai-platform` success |
| GH_TOKEN populated via dd-octo-sts | CI-03 | Requires live CI run with STS | Inspect CI log for `dd-octo-sts token` success |
| claude --bare -p invocable | CI-04 | Requires live CI run with Node/nvm | Inspect CI log for claude version and invocation success |

---

## Validation Sign-Off

- [ ] All tasks have `<automated>` verify or Wave 0 dependencies
- [ ] Sampling continuity: no 3 consecutive tasks without automated verify
- [ ] Wave 0 covers all MISSING references
- [ ] No watch-mode flags
- [ ] Feedback latency < 900s
- [ ] `nyquist_compliant: true` set in frontmatter

**Approval:** pending
