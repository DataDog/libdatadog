# Phase 1: Auth & CI Scaffolding - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-06-15
**Phase:** 1-Auth & CI Scaffolding
**Areas discussed:** Job placement & structure, Claude Code installation, Auth sequence & failure handling, Smoke-test scope

---

## Job Placement & Structure

| Option | Description | Selected |
|--------|-------------|----------|
| New included file (.gitlab/bench-analysis.yml) | Matches benchmarks.yml/fuzz.yml pattern; keeps .gitlab-ci.yml clean | ✓ |
| Directly in .gitlab-ci.yml | Simpler for prototype, mixes concerns | |
| You decide | Claude picks least disruptive option | |

**User's choice:** New included file

| Option | Description | Selected |
|--------|-------------|----------|
| gcp:general-purpose | Standard CI runner, no specialized hardware needed | ✓ |
| apm-k8s-tweaked-metal | Overkill for auth + CLI work | |
| You decide / check with team | Note as detail to verify | |

**User's choice:** gcp:general-purpose

| Option | Description | Selected |
|--------|-------------|----------|
| Every push to any PR branch | Easiest to iterate; stated prototype trigger | ✓ |
| Only when a label is applied | v2 feature, out of scope for v1 | |
| Only on merge_request_event | More targeted, standard GitLab MR trigger | |

**User's choice:** Every push to any PR branch

---

## Claude Code Installation

| Option | Description | Selected |
|--------|-------------|----------|
| nvm + npm install at job start | Matches PHP reference; no custom image needed | ✓ |
| Assume pre-installed in image | Risky — not confirmed | |
| Custom CI image | Cleaner long-term, significant overhead for prototype | |

**User's choice:** nvm install + npm install -g @anthropic-ai/claude-code at job start

| Option | Description | Selected |
|--------|-------------|----------|
| registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1 | Pinned; named in constraints; has dd-octo-sts tools | ✓ |
| Latest tag | Avoids pinning but risks surprise breakage | |
| You decide / verify with infra team | Note for confirmation | |

**User's choice:** Pinned image as stated in constraints

---

## Auth Sequence & Failure Handling

| Option | Description | Selected |
|--------|-------------|----------|
| authanywhere --audience rapid-ai-platform → ANTHROPIC_AUTH_TOKEN | Matches stated constraint exactly | ✓ |
| Check PHP reference for exact flags first | If additional flags needed | |

**User's choice:** authanywhere --audience rapid-ai-platform → ANTHROPIC_AUTH_TOKEN

| Option | Description | Selected |
|--------|-------------|----------|
| Fail the job immediately with clear error | Simplest for prototype; clear signal | ✓ |
| Continue without token, let claude fail | Harder to debug | |
| Post degraded GitHub comment | Complex; requires GitHub auth first | |

**User's choice:** Fail the job immediately with a clear error message

| Option | Description | Selected |
|--------|-------------|----------|
| Create dd-octo-sts policy in Phase 1 | Auth scaffolding is the right place; catches Chainguard coordination issues early | ✓ |
| Defer to Phase 4 | Phase 1 only proves token obtainable | |
| Create stub now, finalize in Phase 4 | Placeholder approach | |

**User's choice:** Create it now in Phase 1

---

## Smoke-Test Scope

| Option | Description | Selected |
|--------|-------------|----------|
| claude --bare -p 'echo hello' exits 0 | Proves CLI installed, token set, AI Gateway reachable | ✓ |
| Check token env vars are non-empty | Proves auth ran, not that token is accepted | |
| Run real prompt, check output | Non-deterministic; harder to assert | |

**User's choice:** claude --bare -p 'echo hello' exits 0

| Option | Description | Selected |
|--------|-------------|----------|
| Full flags matching Phase 3 invocation | Validates exact invocation pattern end-to-end | ✓ |
| Bare minimum (just --bare -p) | Simpler but doesn't validate Phase 3 flags | |

**User's choice:** Full flags (--allowedTools "Read,Write,Glob,Grep" --permission-mode bypassPermissions)

---

## Claude's Discretion

- Exact `rules:` syntax for PR trigger
- nvm version (use latest LTS)

## Deferred Ideas

- Label-based trigger (`benchmark` label) — v2, out of scope for v1
- Custom CI image with Claude Code baked in — v2
- Degraded GitHub comment on auth failure — later phase
