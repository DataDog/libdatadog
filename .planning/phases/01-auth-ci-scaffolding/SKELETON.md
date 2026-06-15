# Walking Skeleton — LLM Benchmark Analysis Pipeline

**Phase:** 1
**Generated:** 2026-06-15

## Capability Proven End-to-End

A GitLab CI job triggers on a libdatadog PR branch, authenticates with both the Datadog AI Gateway (via `authanywhere`) and GitHub (via `dd-octo-sts`), installs Claude Code CLI, and successfully runs `claude --bare -p 'echo hello'` to exit code 0 — proving the full auth-and-invocation stack works before any analysis logic exists.

## Architectural Decisions

| Decision | Choice | Rationale |
|---|---|---|
| CI platform | GitLab CI, included file `.gitlab/bench-analysis.yml` | Matches existing `benchmarks.yml`/`fuzz.yml` modular include pattern (D-01) |
| CI base image | `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` | Pinned per project constraints; bundles dd-octo-sts tooling (D-05) |
| Runner | `gcp:general-purpose` | No specialized hardware needed for auth + CLI invocation (D-02) |
| AI Gateway auth | `authanywhere --audience rapid-ai-platform` → `ANTHROPIC_AUTH_TOKEN` | Datadog-internal Vault OIDC standard; no static Anthropic keys (D-06, CI-02) |
| GitHub auth | `dd-octo-sts token --scope DataDog/libdatadog --policy <name>` → `GH_TOKEN` | Short-lived OIDC federation; no static PATs (D-06, CI-03) |
| dd-octo-sts policy | `.github/chainguard/bench-analysis.write-pr.sts.yaml`, `pull_requests: write`, no `ref` restriction | PR branches can be named anything; created in Phase 1 (D-08, REPORT-03 groundwork) |
| Claude install | nvm `--lts` + `npm install -g @anthropic-ai/claude-code` at job runtime | No custom image for v1 (D-04) |
| Claude invocation | `claude --bare -p` + `--allowedTools "Read,Write,Glob,Grep"` + `--permission-mode bypassPermissions` | Exact flag set Phase 3 will use; proves full invocation path (D-09, CI-04) |
| Trigger | PR-context rules (`$CI_MERGE_REQUEST_IID` / `$CI_EXTERNAL_PULL_REQUEST_IID`) | Repo is GitHub-mirrored; both MR variables covered (D-03) |
| Failure mode | `set -e` + explicit tool probes; fail fast with clear error | No partial runs, no silent continue (D-07) |

## Stack Touched in Phase 1

- [x] Project scaffold — new `.gitlab/bench-analysis.yml` job wired into `.gitlab-ci.yml`
- [x] Auth — real AI Gateway token mint AND real GitHub token federation
- [x] Tooling — real Claude Code CLI install via nvm/npm
- [x] End-to-end invocation — real `claude --bare -p` call returning exit 0
- [x] Run target — runs on GitLab CI on PR branch push; locally validated via `yamllint`

## Out of Scope (Deferred to Later Slices)

- Benchmark fixture data and the jq pre-processor (Phase 2)
- The analysis system prompt and report generation (Phase 3)
- PR comment posting and artifact retention enforcement (Phase 4)
- Label-based or manual triggering (v2)
- Custom CI image with Claude Code pre-baked (v2)
- Degraded GitHub comment on auth failure (later phase)
- Real benchmark runs (`cargo bench`) — relies on provided artifacts only

## Subsequent Slice Plan

Each later phase adds one vertical slice on top of this skeleton without altering its auth or invocation backbone:

- Phase 2: Mock Criterion fixtures + jq pre-processor producing `artifacts/benchmark-diff.json`
- Phase 3: System prompt + Claude invocation producing `artifacts/benchmark-report.md`, with PR diff in context
- Phase 4: Declare report as CI artifact + post/update GitHub PR comment via `gh pr comment`
