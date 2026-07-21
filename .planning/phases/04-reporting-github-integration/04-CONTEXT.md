# Phase 4: Reporting & GitHub Integration - Context

**Gathered:** 2026-06-17
**Status:** Ready for planning

<domain>
## Phase Boundary

Post `artifacts/benchmark-report.md` as a GitHub PR comment using the `gh` CLI and `GH_TOKEN` already wired in Phase 1. Update the comment in place on re-runs (no duplicate notifications). Skip silently when the job runs outside a PR context.

**Already satisfied — do NOT reimplement:**
- REPORT-01: `artifacts/` with `expire_in: 1 month` is live since Phase 3.
- REPORT-03: `.github/chainguard/bench-analysis.write-pr.sts.yaml` exists with `pull_requests:write`, no ref restriction (Phase 1).

**Sole remaining work:** REPORT-02 — a `report.sh` script that posts/updates the GitHub PR comment, wired into `bench-analysis.yml` after `analyze.sh`.

</domain>

<decisions>
## Implementation Decisions

### Posting Tool
- **D-01:** Use `gh` CLI (not raw curl). GH_TOKEN is already exported by the job; `gh` handles auth via that env var automatically. No install step needed (available in `dd-octo-sts-ci-base:2025.06-1`).
- **D-02:** Repo target: `DataDog/libdatadog`. Use `gh api` for comment create/update operations (allows PATCH for update-in-place, which `gh pr comment --edit-last` does not reliably provide).

### PR Number Resolution
- **D-03:** Use `CI_EXTERNAL_PULL_REQUEST_IID` as the PR number. This is the GitHub PR number set by GitLab for mirrored repos. Do NOT use `CI_MERGE_REQUEST_IID` (that is GitLab's internal MR number, wrong for GitHub API).

### Deduplication (Update-in-Place)
- **D-04:** Embed the HTML marker `<!-- bench-analysis-report -->` at the very top of every posted comment body. On each run: list PR comments via `gh api repos/DataDog/libdatadog/issues/${PR_NUMBER}/comments`, find the one containing the marker (using `jq`), then PATCH it via `gh api --method PATCH`. If none found, POST a new comment.

### Non-PR Context
- **D-05:** When `CI_EXTERNAL_PULL_REQUEST_IID` is not set (direct branch push with no open PR): log a short message (`"No PR number found — skipping GitHub comment"`) and `exit 0`. The artifact is still saved; no job failure.

### Comment Format
- **D-06:** Wrap the report body in a `<details>` collapsible block. The `<summary>` line shows the verdict extracted from `artifacts/benchmark-report.md` (e.g., `🟢 Benchmark Analysis: pass` / `🟡 warn` / `🔴 fail`). The HTML marker goes before the `<details>` tag.
- **D-07:** Verdict extraction: `grep -m1 '^### Verdict' -A2 artifacts/benchmark-report.md | tail -1` (or similar one-liner) to pull the single verdict word from the report. Map `pass→🟢`, `warn→🟡`, `fail→🔴`. Fall back to `📊` if extraction fails.
- **D-08:** No timestamp or CI run link in the summary — keep the header clean. The report body already contains the details.

### Claude's Discretion
- Exact `gh api` flag set and JSON payload construction for POST vs PATCH.
- Exact `jq` filter to identify the marker comment from the list response.
- Whether to use `--jq` on `gh api` or pipe to `jq` separately.

</decisions>

<canonical_refs>
## Canonical References

**Downstream agents MUST read these before planning or implementing.**

### Existing CI Infrastructure
- `.gitlab/bench-analysis.yml` — the CI job to modify; `report.sh` is added as a script step after `analyze.sh`; `GH_TOKEN` and `CI_EXTERNAL_PULL_REQUEST_IID` are already available
- `.github/chainguard/bench-analysis.write-pr.sts.yaml` — the dd-octo-sts policy granting `pull_requests:write` (REPORT-03, already done — read to confirm scope)
- `.gitlab/bench-analysis/analyze.sh` — direct analog for script structure (strict mode, env-var-overridable paths, pre-condition guard, non-empty assertion)

### Requirements
- `.planning/REQUIREMENTS.md` — REPORT-01, REPORT-02, REPORT-03 acceptance criteria
- `.planning/ROADMAP.md` §Phase 4 — success criteria (3 items)

### Phase Context (prior decisions)
- `.planning/phases/01-auth-ci-scaffolding/01-CONTEXT.md` — D-08 (policy created in Phase 1), D-03 (both MR variables present in CI)
- `.planning/phases/03-claude-analysis/03-01-PLAN.md` — confirms `artifacts/` artifact path and `expire_in: 1 month` already set

</canonical_refs>

<code_context>
## Existing Code Insights

### Reusable Assets
- `.gitlab/bench-analysis/analyze.sh` — exact structural analog: `#!/usr/bin/env bash`, `set -euo pipefail`, env-var-overridable defaults (`REPORT="${REPORT:-artifacts/benchmark-report.md}"`), pre-condition guard (`[ ! -s "${REPORT}" ]`), NVM sourcing block. Copy this structure for `report.sh`.
- `.gitlab/bench-analysis/preprocess.sh` — same pattern; confirms the non-empty guard convention used across the pipeline.

### Established Patterns
- All CI scripts in `.gitlab/bench-analysis/` use `set -euo pipefail`, `SCRIPT_DIR` via `BASH_SOURCE[0]`, and env-var-overridable paths.
- GH_TOKEN is minted at job start and exported — `report.sh` can use it directly without re-minting.
- `gh` CLI: authenticated by `GH_TOKEN` env var automatically; no `gh auth login` needed in CI.

### Integration Points
- `bench-analysis.yml` script block: add `bash .gitlab/bench-analysis/report.sh` after `bash .gitlab/bench-analysis/analyze.sh`.
- `artifacts/` block already covers `benchmark-report.md` — no change needed.

</code_context>

<specifics>
## Specific Ideas

- The HTML marker `<!-- bench-analysis-report -->` must be the very first line of the comment body so `grep`/`jq` finds it reliably.
- Verdict extraction from the report should be fault-tolerant — fall back to a generic `📊 Benchmark Analysis` summary if the grep fails rather than erroring the whole script.
- The `<details>` block keeps the PR comment thread clean since the report can be 50–400 lines.

</specifics>

<deferred>
## Deferred Ideas

- Truncated comment with link to CI artifact URL — requires constructing a GitLab artifact URL, deferred to v2.
- Timestamp or CI run link in the comment summary — deferred; adds complexity for marginal value in v1.

</deferred>

---

*Phase: 4-Reporting & GitHub Integration*
*Context gathered: 2026-06-17*
