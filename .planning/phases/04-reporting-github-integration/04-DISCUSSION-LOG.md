# Phase 4: Reporting & GitHub Integration — Discussion Log

**Session:** 2026-06-17
**Areas discussed:** Comment posting tool, Deduplication strategy, Non-PR context, Comment format

---

## Comment posting tool

| Question | Options presented | Selected | Notes |
|----------|-------------------|----------|-------|
| How should the report be posted to GitHub? | gh CLI / curl + GitHub API / You decide | **gh CLI** | GH_TOKEN already exported; gh handles auth automatically |
| Which PR number variable to use? | CI_EXTERNAL_PULL_REQUEST_IID / CI_MERGE_REQUEST_IID / You decide | **CI_EXTERNAL_PULL_REQUEST_IID** | GitHub PR number for mirrored repos; MR IID is GitLab-internal |

## Deduplication strategy

| Question | Options presented | Selected | Notes |
|----------|-------------------|----------|-------|
| How to identify existing comment? | HTML marker / Fixed prefix / Delete+recreate | **HTML marker** | `<!-- bench-analysis-report -->` at top of body; find via gh api + jq, then PATCH |

## Non-PR context

| Question | Options presented | Selected | Notes |
|----------|-------------------|----------|-------|
| What to do when no PR number is set? | Skip silently exit 0 / Warn and continue / Fail the job | **Skip silently exit 0** | Log message + exit 0; artifact still saved |

## Comment format

| Question | Options presented | Selected | Notes |
|----------|-------------------|----------|-------|
| How to present the report? | Full verbatim / Collapsible details / Truncated with link | **Collapsible details block** | Keeps PR thread tidy for 50–400 line reports |
| Summary line content? | Verdict only / Verdict + CI link / Generic title | **Verdict only** | Extract pass/warn/fail from report; map to 🟢/🟡/🔴 |

## Deferred ideas

- Truncated comment with CI artifact URL link — v2
- Timestamp or CI run link in summary — v2
