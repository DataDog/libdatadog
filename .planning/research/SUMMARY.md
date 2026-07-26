# Research Summary — LLM Benchmark Analysis Pipeline

## Executive Summary

This project adds a GitLab CI job to libdatadog that invokes Claude (via Datadog's AI Gateway) to analyze Criterion (Rust micro) and dd-trace-py (Python macro) benchmark results and post a structured performance report as a GitHub PR comment. The reference implementation is dd-trace-py's `summarize_failures.py` — the same auth chain, env var names, `authanywhere` binary, and headless Claude Code CLI invocation pattern apply directly.

Key architectural insight: a shell + `jq` pre-processor owns all numeric computation (deltas, % changes, regression classification), and Claude only produces natural-language interpretation. The system prompt lives in a separate `.md` file.

## Stack

| Tool | Role | Notes |
|------|------|-------|
| `claude --bare -p` | Headless LLM | `--bare` for deterministic CI; do NOT use `--dangerously-skip-permissions` |
| `authanywhere --audience rapid-ai-platform` | AI Gateway auth | OIDC bearer token — fetch immediately before use, not at job start |
| `dd-octo-sts token` | GitHub auth | Short-lived `GH_TOKEN`; no static PATs |
| `cargo-criterion --message-format=json` + `critcmp --export` | Criterion output | Official machine-readable format; do NOT parse `target/criterion/` (unstable) |
| `jq` | Pre-processor | Keeps all arithmetic out of the LLM |
| `ANTHROPIC_AUTH_TOKEN` + `ANTHROPIC_BASE_URL` | AI Gateway config | Do NOT set `ANTHROPIC_API_KEY` alongside `ANTHROPIC_AUTH_TOKEN` |

## Table Stakes Features

- Overall pass/warn/fail verdict keyed to a configurable % threshold
- Per-benchmark % change with absolute before/after values
- Noise guard using Criterion confidence intervals (changes within CI = not a regression)
- Three sections: regressions / improvements / unchanged (unchanged collapsed)
- Suite labeling (Criterion vs dd-trace-py)
- Raw artifact link in footer

## Differentiators (LLM value-add)

- Natural-language regression summary grounded in data only
- Suspect code change pointer: PR diff fed to LLM, names files/functions overlapping with regressing benchmarks
- Grouped by benchmark ID prefix for readability
- Improvement callout (often skipped by static tools)

## Anti-features (deliberately exclude)

- Full benchmark table in comment body — use `<details>` fold or artifact link
- Flame graphs in PR comment — don't render in GitHub
- Trend-over-time graphs — separate workstream
- Automated PR blocking — unsafe until benchmarks run on dedicated runners
- LLM hedging language ("I think", confidence scores)

## Architecture — Four Shell Scripts

1. `setup-bench-auth.sh` — `authanywhere` → `ANTHROPIC_AUTH_TOKEN`; `dd-octo-sts` → `GH_TOKEN`
2. `process-benchmarks.sh` (+ jq) — computes deltas → `artifacts/benchmark-diff.json`
3. `invoke-claude.sh` — `claude --bare -p` with system prompt file, `--allowedTools Read,Write,Glob,Grep`
4. `post-bench-comment.sh` — `gh pr comment` with update semantics (no comment proliferation)

## Top Pitfalls

1. **authanywhere token expiry** — fetch immediately before Claude invocation, not at job start
2. **dd-octo-sts policy missing PR branch access** — new policy file needed in `.github/chainguard/`; requires coordination with security team
3. **Claude produces no output file, silently exits 0** — assert `[ -s artifacts/benchmark-report.md ]` after Claude; `Write` must be in `--allowedTools`
4. **LLM hallucination of causes** — system prompt must say: "Do not explain why a regression occurred unless visible in the diff or benchmark name"
5. **Machine variance flagged as regression** — use Criterion's `change.mean.confidence_interval`, not just mean; mock data must include noisy-but-within-CI benchmarks
6. **Secret leakage** — never `set -x` in the block setting `ANTHROPIC_AUTH_TOKEN`

## Suggested Phase Order

1. Auth and Environment Scaffolding
2. Mock Data and Pre-processor
3. Claude Invocation and Report Generation
4. PR Comment Posting and CI Integration
5. Real Benchmark Data Integration

## Open Questions

1. **dd-trace-py benchmark output format** — `bm.Scenario` non-profiling schema is undocumented; prototype with mocked data, document as a contract
2. **`authanywhere` availability** — verify with `which authanywhere` in a throwaway CI job against `dd-octo-sts-ci-base:2025.06-1`
3. **dd-octo-sts policy for PR branches** — may require Chainguard team coordination; identify this as a cross-team dependency early
