# Prophylactic Benchmarking — LLM Analysis Pipeline

## What This Is

A GitLab CI job in libdatadog that uses Claude (via Datadog's AI Gateway) to analyze benchmark results and post AI-augmented performance reports directly onto libdatadog GitHub PRs. It compares the PR branch against libdatadog `main` to surface regressions, improvements, and suspect code changes — giving contributors instant feedback without waiting for the downstream release cycle.

This is the **"Use LLMs to analyze performance data"** piece of the broader prophylactic benchmarking initiative. The other pieces (cross-repo benchmark triggering, dd-trace-py auto-update) are parallel workstreams by other team members.

## Core Value

Contributors get benchmark impact feedback on their libdatadog PR before merge, not after a full release cycle.

## Requirements

### Validated

(None yet — ship to validate)

### Active

- [ ] GitLab CI job authenticated with the Datadog AI Gateway via Vault JWT
- [ ] Claude Code CLI installed and invocable in the CI environment
- [ ] System prompt that produces a benchmark analysis report (global summary, regression/improvement detection, suspect code pointer)
- [ ] Report posted as a GitHub PR comment (via `gh` or GitHub API + dd-octo-sts token)
- [ ] Report saved as a CI artifact (Markdown)
- [ ] Mock benchmark data covering both Criterion (Rust micro) and dd-trace-py (macro) formats so the pipeline can be tested end-to-end without real benchmark runs
- [ ] Comparison baseline: PR branch vs libdatadog `main`

### Out of Scope

- Triggering actual benchmarks (Augusto's workstream)
- Running dd-trace-py benchmark suite from this CI job
- Continuous benchmarking from `main` (follow-up)
- Automated perf improvement loop (follow-up)
- Competitor / macro benchmarks beyond dd-trace-py

## Context

libdatadog is upstream of all Datadog tracer libraries. Once something merges and releases, benchmarking happens downstream — bundled with many unrelated changes, making it hard to attribute regressions or validate improvements. The goal is to short-circuit this by surfacing benchmark data on the PR itself.

The PHP team already does something similar for integration testing (`dd-trace-php/.gitlab/libdatadog-latest.yml`): a GitLab CI job uses Vault JWT → BTI token, installs Claude Code, and invokes it non-interactively with `--allowedTools` and `--permission-mode bypassPermissions`. This is the reference implementation pattern.

The AI Gateway endpoint is `https://ai-gateway.us1.ddbuild.io` with custom headers (source, org-id, provider, claude-code, Authorization Bearer).

Benchmark formats to handle:
- **Criterion** (Rust micro): JSON output from `cargo bench --message-format=json` or the `criterion` HTML/JSON reports
- **dd-trace-py** (macro): format TBD pending Augusto's triggering work; prototype with mocked data

## Constraints

- Must use Datadog AI Gateway (not direct Anthropic API keys)
- Auth via Vault OIDC JWT → `rapid-ai-platform` audience (same as PHP reference)
- CI image: `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` or similar
- GitHub PR comments require dd-octo-sts token scoped to `DataDog/libdatadog`
- No root in CI — install Node/Claude Code via nvm if not pre-installed
- Prototype triggers on every push to a PR branch for easy iteration

## Key Decisions

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Use Claude Code CLI (not direct Anthropic API) | Matches PHP reference pattern; allows `--allowedTools` and file access inside CI | Decided |
| Mock benchmark data first | Triggering is a separate workstream; unblocks pipeline development | Decided |
| Scope to both micro + macro formats from the start | Avoids rework when macro triggering lands | Decided |
| System prompt (not a packaged skill) | Sufficient for the analysis task; simpler to iterate | Decided |

## Evolution

This document evolves at phase transitions and milestone boundaries.

**After each phase transition** (via `/gsd-transition`):
1. Requirements invalidated? → Move to Out of Scope with reason
2. Requirements validated? → Move to Validated with phase reference
3. New requirements emerged? → Add to Active
4. Decisions to log? → Add to Key Decisions
5. "What This Is" still accurate? → Update if drifted

**After each milestone** (via `/gsd-complete-milestone`):
1. Full review of all sections
2. Core Value check — still the right priority?
3. Audit Out of Scope — reasons still valid?
4. Update Context with current state

---
*Last updated: 2026-06-15 after initialization*
