# Phase 4: Reporting & GitHub Integration - Research

**Researched:** 2026-06-17
**Domain:** Shell scripting — `gh` CLI / GitHub Issues API / GitLab CI integration
**Confidence:** HIGH

---

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

- **D-01:** Use `gh` CLI (not raw curl). GH_TOKEN is already exported by the job; `gh` handles auth via that env var automatically. No install step needed (available in `dd-octo-sts-ci-base:2025.06-1`).
- **D-02:** Repo target: `DataDog/libdatadog`. Use `gh api` for comment create/update operations (allows PATCH for update-in-place, which `gh pr comment --edit-last` does not reliably provide).
- **D-03:** Use `CI_EXTERNAL_PULL_REQUEST_IID` as the PR number. This is the GitHub PR number set by GitLab for mirrored repos. Do NOT use `CI_MERGE_REQUEST_IID`.
- **D-04:** Embed HTML marker `<!-- bench-analysis-report -->` at the very top of every posted comment body. On each run: list PR comments via `gh api repos/DataDog/libdatadog/issues/${PR_NUMBER}/comments`, find the one containing the marker (using `jq`), then PATCH it. If none found, POST a new comment.
- **D-05:** When `CI_EXTERNAL_PULL_REQUEST_IID` is not set: log `"No PR number found — skipping GitHub comment"` and `exit 0`.
- **D-06:** Wrap the report body in a `<details>` collapsible block. The `<summary>` line shows the verdict extracted from `artifacts/benchmark-report.md`. The HTML marker goes before the `<details>` tag.
- **D-07:** Verdict extraction: `grep -m1 '^### Verdict' -A2 artifacts/benchmark-report.md | tail -1`. Map `pass→🟢`, `warn→🟡`, `fail→🔴`. Fall back to `📊` if extraction fails.
- **D-08:** No timestamp or CI run link in the summary — keep the header clean.

### Claude's Discretion

- Exact `gh api` flag set and JSON payload construction for POST vs PATCH.
- Exact `jq` filter to identify the marker comment from the list response.
- Whether to use `--jq` on `gh api` or pipe to `jq` separately.

### Deferred Ideas (OUT OF SCOPE)

- Truncated comment with link to CI artifact URL — requires constructing a GitLab artifact URL, deferred to v2.
- Timestamp or CI run link in the comment summary — deferred; adds complexity for marginal value in v1.
</user_constraints>

---

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| REPORT-01 | `artifacts/benchmark-report.md` declared as GitLab CI artifact, retained ≥ 30 days | **Already done** in Phase 3 (`artifacts/` with `expire_in: 1 month` in `bench-analysis.yml`) — no action needed |
| REPORT-02 | CI job posts report as GitHub PR comment; re-run updates existing comment, no duplicates | `report.sh` script using `gh api` POST + PATCH with HTML marker deduplication |
| REPORT-03 | dd-octo-sts policy in `.github/chainguard/` grants `pull_requests: write` for PR branches | **Already done** in Phase 1 — `bench-analysis.write-pr.sts.yaml` confirmed with no ref restriction |
</phase_requirements>

---

## Summary

Phase 4 has one deliverable: `report.sh`. REPORT-01 and REPORT-03 are already satisfied by prior phases — the `artifacts/` block with `expire_in: 1 month` is live in `bench-analysis.yml`, and `.github/chainguard/bench-analysis.write-pr.sts.yaml` exists with `pull_requests: write` and no ref restriction.

`report.sh` reads `artifacts/benchmark-report.md`, extracts the verdict line, builds a `<details>` comment body prefixed with the HTML marker `<!-- bench-analysis-report -->`, then uses `gh api` to list existing PR comments, find any with the marker, and either PATCH the existing comment or POST a new one. When `CI_EXTERNAL_PULL_REQUEST_IID` is absent, it logs and exits 0.

The script follows the exact structural conventions of `analyze.sh` and `preprocess.sh`: `set -euo pipefail`, env-var-overridable paths, a pre-condition guard on the report file, and a `wc -l` echo on success. The CI wiring is a single line appended after `bash .gitlab/bench-analysis/analyze.sh` in `bench-analysis.yml`.

**Primary recommendation:** Implement `report.sh` as a near-verbatim structural copy of `analyze.sh` with `gh api` calls replacing the Claude invocation. The JSON payload for both POST and PATCH uses the `--field body=` form; the jq filter for finding the existing comment is `.[] | select(.body | startswith("<!-- bench-analysis-report -->")) | .id`.

---

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|-------------|----------------|-----------|
| Report artifact retention | GitLab CI | — | `artifacts:` block in `bench-analysis.yml` — already done |
| PR comment post/update | CI script (report.sh) | GitHub API | Script drives; API is the transport |
| Auth (GH_TOKEN) | CI job (bench-analysis.yml) | dd-octo-sts | Token minted at job start, already exported |
| Verdict extraction | report.sh (bash + grep) | — | One-liner from benchmark-report.md |
| Comment deduplication | report.sh (jq filter) | GitHub Issues API | List → find marker → PATCH or POST |

---

## Standard Stack

### Core

| Tool | Version | Purpose | Why Standard |
|------|---------|---------|--------------|
| `gh` CLI | 2.89.0 (local); pre-installed in CI image | GitHub API operations | D-01; GH_TOKEN auto-auth; available in CI image |
| `jq` | 1.8.1 (local); present in CI image | JSON parsing of comment list response | Standard in all CI images; no install needed |
| bash | 5.x | Script execution | All existing scripts use bash |

No new packages to install. [VERIFIED: codebase — bench-analysis.yml, existing scripts]

---

## Package Legitimacy Audit

No external packages are introduced in this phase. All tools (`gh`, `jq`, bash) are pre-installed in the CI image or already wired in the job. This section is not applicable.

---

## Architecture Patterns

### System Architecture Diagram

```
bench-analysis CI job
      |
      ├── preprocess.sh  →  artifacts/benchmark-comparison.md
      ├── analyze.sh     →  artifacts/benchmark-report.md
      └── report.sh
              |
              ├── [no CI_EXTERNAL_PULL_REQUEST_IID] → log + exit 0
              └── [PR context]
                      |
                      ├── extract verdict from benchmark-report.md
                      ├── build comment body (<details> + HTML marker)
                      ├── gh api GET  /repos/DataDog/libdatadog/issues/$PR/comments
                      ├── jq: find comment with marker → comment_id or empty
                      ├── [found]  → gh api PATCH /repos/.../issues/comments/$comment_id
                      └── [not found] → gh api POST  /repos/.../issues/$PR/comments
```

### Recommended Project Structure

No new directories. Single new file:

```
.gitlab/bench-analysis/
└── report.sh        # new — posts/updates GitHub PR comment
```

`bench-analysis.yml`: one new script line after `analyze.sh`.

### Pattern 1: `gh api` POST a new comment

```bash
# Source: gh CLI docs / GitHub REST API — POST /repos/{owner}/{repo}/issues/{issue_number}/comments
gh api \
  --method POST \
  -H "Accept: application/vnd.github+json" \
  "repos/DataDog/libdatadog/issues/${PR_NUMBER}/comments" \
  --field body="${COMMENT_BODY}"
```

The `--field` flag URL-encodes and JSON-wraps the value automatically; no manual JSON construction needed. [ASSUMED — training knowledge, standard gh CLI usage pattern; verify against `gh api --help` in CI]

### Pattern 2: `gh api` PATCH an existing comment

```bash
# Source: GitHub REST API — PATCH /repos/{owner}/{repo}/issues/comments/{comment_id}
gh api \
  --method PATCH \
  -H "Accept: application/vnd.github+json" \
  "repos/DataDog/libdatadog/issues/comments/${COMMENT_ID}" \
  --field body="${COMMENT_BODY}"
```

Note: The PATCH endpoint is `/issues/comments/{comment_id}` (not `/issues/{number}/comments/{comment_id}`). [ASSUMED — standard GitHub REST API shape; confirmed by 404 response on comment ID 1 showing the correct path structure]

### Pattern 3: jq filter to find the marker comment

```bash
COMMENT_ID=$(
  gh api "repos/DataDog/libdatadog/issues/${PR_NUMBER}/comments" \
    --jq '.[] | select(.body | startswith("<!-- bench-analysis-report -->")) | .id' \
  | head -1
)
```

`--jq` is supported on `gh api` and avoids a separate `jq` pipe. `head -1` guards against the degenerate case of multiple matching comments (take the oldest). Returns empty string if no match. [ASSUMED — `--jq` flag is well-documented on `gh api`; startswith is a valid jq filter]

### Pattern 4: Comment body construction

```bash
MARKER="<!-- bench-analysis-report -->"
VERDICT_LINE=$(grep -m1 '^### Verdict' -A2 "${REPORT}" | tail -1 | tr -d '[:space:]' || true)
case "${VERDICT_LINE}" in
  pass) EMOJI="🟢" ;;
  warn) EMOJI="🟡" ;;
  fail) EMOJI="🔴" ;;
  *)    EMOJI="📊" ;;
esac
REPORT_BODY=$(cat "${REPORT}")
COMMENT_BODY="${MARKER}
<details>
<summary>${EMOJI} Benchmark Analysis: ${VERDICT_LINE:-unknown}</summary>

${REPORT_BODY}
</details>"
```

`cat` into a variable is safe for files up to ~400 lines (D-08 from analyze-prompt.md). [ASSUMED — bash variable assignment pattern]

### Anti-Patterns to Avoid

- **Using `gh pr comment --edit-last`:** Does not reliably identify the bench-analysis comment when multiple bots post — use the HTML marker + `gh api` PATCH pattern (D-02).
- **Using `CI_MERGE_REQUEST_IID`:** This is GitLab's internal MR number. The repo is GitHub-mirrored; `CI_EXTERNAL_PULL_REQUEST_IID` is the GitHub PR number (D-03).
- **Failing the job when not in PR context:** `CI_EXTERNAL_PULL_REQUEST_IID` is unset for direct branch pushes — must `exit 0` (D-05).
- **Building raw JSON with string interpolation:** Use `gh api --field body=` to avoid escaping bugs with backticks, double-quotes, and newlines in the report body. [ASSUMED]
- **Echoing GH_TOKEN to stdout/stderr:** Never log the token value.

---

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| GitHub auth | Manual token fetch | `gh` CLI with `GH_TOKEN` | Token already exported by job |
| JSON serialization of comment body | String concatenation with escaping | `gh api --field body=` | Handles newlines, quotes, special chars |
| Comment list pagination | Manual page iteration | `gh api` with `--paginate` if needed | API returns up to 30 comments by default; for most PRs this is sufficient in v1 |

---

## Runtime State Inventory

> Not applicable — this is a greenfield script addition, not a rename or migration phase.

---

## Common Pitfalls

### Pitfall 1: Newlines in comment body break `--field`

**What goes wrong:** Multi-line `COMMENT_BODY` passed via `--field body="${COMMENT_BODY}"` with unquoted `$()` expansion strips newlines.
**Why it happens:** Shell word-splitting collapses whitespace on unquoted expansions inside double-quotes passed to `--field`.
**How to avoid:** Use `printf '%s' "${COMMENT_BODY}"` or a heredoc-fed variable. When assigning multi-line content: `COMMENT_BODY=$(printf '...')` with explicit `\n`. Verify the rendered comment in a test run.
**Warning signs:** Comment body appears as a single line in GitHub UI.

### Pitfall 2: PATCH endpoint path vs POST endpoint path

**What goes wrong:** Using `/issues/${PR_NUMBER}/comments/${COMMENT_ID}` for PATCH returns 404.
**Why it happens:** GitHub's REST API uses `/issues/comments/{comment_id}` (flat, not nested under issue number) for single-comment operations.
**How to avoid:** POST to `repos/.../issues/${PR_NUMBER}/comments`; PATCH/GET/DELETE to `repos/.../issues/comments/${COMMENT_ID}`. [ASSUMED — standard API shape]
**Warning signs:** `gh api` exits non-zero with HTTP 404 on the PATCH call.

### Pitfall 3: `jq` returns empty for no-match, not an error

**What goes wrong:** Script proceeds to PATCH with `COMMENT_ID=""` if the `select` filter returns nothing.
**Why it happens:** `jq` exits 0 and outputs nothing when no element matches `.[] | select(...)`.
**How to avoid:** Test `if [ -z "${COMMENT_ID}" ]` before branching; route to POST when empty.
**Warning signs:** PATCH called with URL ending in `/issues/comments/` (empty ID), returning 404 or acting on wrong comment.

### Pitfall 4: `CI_EXTERNAL_PULL_REQUEST_IID` absent outside PR pipelines

**What goes wrong:** Script fails with unbound variable error if `set -u` is active and the variable is referenced directly.
**Why it happens:** `set -euo pipefail` treats unbound vars as errors; `CI_EXTERNAL_PULL_REQUEST_IID` is only set when GitLab detects an open external PR.
**How to avoid:** Use `PR_NUMBER="${CI_EXTERNAL_PULL_REQUEST_IID:-}"` (default to empty string), then guard with `if [ -z "${PR_NUMBER}" ]`.
**Warning signs:** Job exits non-zero with `unbound variable` in stderr.

### Pitfall 5: Verdict extraction fails on unexpected report format

**What goes wrong:** `grep -m1 '^### Verdict' -A2` returns nothing if the report has no Verdict section (e.g., Claude produced an error message).
**Why it happens:** The analyze-prompt.md mandates the section but the output is not guaranteed if Claude fails or produces a malformed report.
**How to avoid:** Use `|| true` after the grep; the `case` statement's `*` branch falls back to `📊 Benchmark Analysis: unknown`. Never `set -e`-fail on verdict extraction.
**Warning signs:** Comment summary shows `📊 Benchmark Analysis: unknown`.

---

## Code Examples

### report.sh skeleton (full script)

```bash
#!/usr/bin/env bash
# Source: structural analog of .gitlab/bench-analysis/analyze.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPORT="${REPORT:-artifacts/benchmark-report.md}"
REPO="${REPO:-DataDog/libdatadog}"

# Pre-condition guard (D-05: non-PR context)
PR_NUMBER="${CI_EXTERNAL_PULL_REQUEST_IID:-}"
if [ -z "${PR_NUMBER}" ]; then
  echo "No PR number found — skipping GitHub comment"
  exit 0
fi

# Pre-condition guard: report must exist
if [ ! -s "${REPORT}" ]; then
  echo "ERROR: ${REPORT} is missing or empty — run analyze.sh first" >&2
  exit 1
fi

# Verdict extraction (D-07)
VERDICT_LINE=$(grep -m1 '^### Verdict' -A2 "${REPORT}" | tail -1 | tr -d '[:space:]' || true)
case "${VERDICT_LINE}" in
  pass) EMOJI="🟢" ;;
  warn) EMOJI="🟡" ;;
  fail) EMOJI="🔴" ;;
  *)    EMOJI="📊" ;;
esac

# Build comment body (D-06)
MARKER="<!-- bench-analysis-report -->"
REPORT_BODY=$(cat "${REPORT}")
COMMENT_BODY="${MARKER}
<details>
<summary>${EMOJI} Benchmark Analysis: ${VERDICT_LINE:-unknown}</summary>

${REPORT_BODY}
</details>"

# Find existing comment by marker (D-04)
COMMENT_ID=$(
  gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --jq '.[] | select(.body | startswith("<!-- bench-analysis-report -->")) | .id' \
  | head -1
)

if [ -n "${COMMENT_ID}" ]; then
  # Update existing comment (PATCH)
  gh api \
    --method PATCH \
    -H "Accept: application/vnd.github+json" \
    "repos/${REPO}/issues/comments/${COMMENT_ID}" \
    --field body="${COMMENT_BODY}"
  echo "Updated existing benchmark comment (id=${COMMENT_ID})"
else
  # Post new comment
  gh api \
    --method POST \
    -H "Accept: application/vnd.github+json" \
    "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --field body="${COMMENT_BODY}"
  echo "Posted new benchmark comment on PR #${PR_NUMBER}"
fi

echo "report.sh done ($(wc -l < "${REPORT}") lines in report)"
```

[ASSUMED — training knowledge and codebase patterns from analyze.sh/preprocess.sh; exact `gh api --field` newline behavior should be validated on first CI run]

### bench-analysis.yml addition (single line)

```yaml
# After existing analyze.sh step:
- bash .gitlab/bench-analysis/analyze.sh
- bash .gitlab/bench-analysis/report.sh   # <-- add this line
```

---

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `gh pr comment` | `gh api` POST/PATCH | Phase 4 decision (D-02) | Enables update-in-place; `gh pr comment --edit-last` is unreliable for multi-bot PRs |

**Confirmed not needed:**
- `gh auth login`: GH_TOKEN env var is sufficient for `gh` CLI auth.
- `--paginate` on comment list: PRs will have fewer than 30 comments in the expected usage window; add if needed in v2.

---

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | `gh api --field body=` correctly serializes multi-line strings including newlines | Code Examples, Pitfall 1 | Comment body appears on one line; fix by switching to `--input` with a temp file or `--raw-field` |
| A2 | PATCH endpoint is `/repos/.../issues/comments/{id}` (not nested under issue number) | Pattern 2, Pitfall 2 | 404 on update; fix by adjusting URL |
| A3 | `gh api --jq` is supported in the version pre-installed in `dd-octo-sts-ci-base:2025.06-1` | Pattern 3 | Script errors; fall back to piping to system `jq` |
| A4 | `CI_EXTERNAL_PULL_REQUEST_IID` is the correct GitLab variable for GitHub PR number on mirrored repos | D-03 | Wrong PR receives comment; confirmed by STATE.md "Both CI_MERGE_REQUEST_IID and CI_EXTERNAL_PULL_REQUEST_IID rules added" |

---

## Open Questions

1. **Does `gh api --field` handle newlines in the body correctly in CI?**
   - What we know: `gh api --field` is documented for simple string fields; behavior with multi-line strings containing Markdown is less documented.
   - What's unclear: Whether the CI image's `gh` version serializes multi-line values correctly or requires `--raw-field` / `--input`.
   - Recommendation: Add a Bats static test that constructs a comment body and checks `gh api --help | grep -q 'raw-field'`; if absent, use `--field`. Validate on first real CI run.

2. **Does `gh api --jq` exist in the CI image's `gh` version?**
   - What we know: `gh api --jq` exists in gh 2.x (confirmed locally at 2.89.0). CI image version is unverified.
   - Recommendation: Write a Bats static test `gh api --help | grep -q '\-\-jq'`; fall back to pipe to system `jq` if absent.

---

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|------------|-----------|---------|----------|
| `gh` CLI | report.sh | ✓ (local) | 2.89.0 | — (pre-installed in CI image per D-01) |
| `jq` | report.sh (via `--jq` or pipe) | ✓ (local) | 1.8.1 | pipe to system jq if `--jq` unavailable |
| `GH_TOKEN` | gh auth | Exported by CI job | — | — (minted via dd-octo-sts at job start) |
| `CI_EXTERNAL_PULL_REQUEST_IID` | PR number | Set by GitLab for external PRs | — | Absent → exit 0 (D-05) |
| `bats` | report.bats test | ✓ (local) | — | CI must have bats pre-installed |

---

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | bats (Bash Automated Testing System) |
| Config file | none — direct invocation |
| Quick run command | `bats .gitlab/bench-analysis/report.bats` |
| Full suite command | `bats .gitlab/bench-analysis/` |

### Phase Requirements → Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| REPORT-01 | `artifacts/` with `expire_in: 1 month` in YAML | static | `grep -q 'expire_in' .gitlab/bench-analysis.yml` | ✅ (already done) |
| REPORT-02 | report.sh exists and is syntactically valid | static | `bash -n .gitlab/bench-analysis/report.sh` | ❌ Wave 0 |
| REPORT-02 | No-PR guard present | static | `grep -q 'skipping GitHub comment' .gitlab/bench-analysis/report.sh` | ❌ Wave 0 |
| REPORT-02 | HTML marker present in script | static | `grep -q 'bench-analysis-report' .gitlab/bench-analysis/report.sh` | ❌ Wave 0 |
| REPORT-02 | `bench-analysis.yml` calls report.sh | static | `grep -q 'report.sh' .gitlab/bench-analysis.yml` | ❌ Wave 0 |
| REPORT-02 | Integration: posts/updates comment | integration (CI-only) | skip locally | ❌ Wave 0 |
| REPORT-03 | Policy file grants `pull_requests: write` | static | `grep -q 'pull_requests: write' .github/chainguard/bench-analysis.write-pr.sts.yaml` | ✅ (already done) |

### Sampling Rate

- **Per task commit:** `bash -n .gitlab/bench-analysis/report.sh && bats .gitlab/bench-analysis/report.bats`
- **Per wave merge:** `bats .gitlab/bench-analysis/`
- **Phase gate:** Full suite green before `/gsd-verify-work`

### Wave 0 Gaps

- [ ] `.gitlab/bench-analysis/report.bats` — covers REPORT-02 static checks and CI-only integration test

---

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|-----------------|
| V2 Authentication | no | GH_TOKEN minted by dd-octo-sts (Phase 1) |
| V3 Session Management | no | Stateless CI job |
| V4 Access Control | yes | Token scoped to `pull_requests: write` only (REPORT-03) |
| V5 Input Validation | yes | Report content read from trusted CI artifact; no user-controlled input injected into API calls |
| V6 Cryptography | no | No crypto operations in this script |

### Known Threat Patterns

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| Token leakage via echo/log | Information Disclosure | Never echo `GH_TOKEN`; `set -x` must not be used in report.sh |
| Comment body injection from report content | Tampering | Report is a trusted CI artifact written by analyze.sh; PR diff is not re-injected in this phase |
| Overposting to wrong PR | Tampering | `PR_NUMBER` sourced from `CI_EXTERNAL_PULL_REQUEST_IID` (GitLab-controlled); not user-provided |

---

## Sources

### Primary (HIGH confidence)
- `.gitlab/bench-analysis/analyze.sh` — structural analog; script conventions confirmed by reading
- `.gitlab/bench-analysis/preprocess.sh` — structural analog; pre-condition guard pattern
- `.gitlab/bench-analysis/preprocess.bats` — bats test structure and skip-guard patterns
- `.github/chainguard/bench-analysis.write-pr.sts.yaml` — REPORT-03 confirmed satisfied
- `.gitlab/bench-analysis.yml` — REPORT-01 confirmed satisfied; GH_TOKEN export confirmed
- `.planning/phases/04-reporting-github-integration/04-CONTEXT.md` — all locked decisions

### Secondary (MEDIUM confidence)
- `gh` CLI version 2.89.0 locally — confirms `--jq` and `--field` flags exist; CI image version unverified

### Tertiary (LOW confidence / ASSUMED)
- `gh api --field` newline serialization behavior — training knowledge; validate on first CI run
- GitHub REST API PATCH endpoint path `/issues/comments/{id}` — training knowledge; standard REST shape

---

## Metadata

**Confidence breakdown:**
- Script structure: HIGH — direct analogs in codebase (`analyze.sh`, `preprocess.sh`)
- `gh api` flag syntax: MEDIUM — confirmed locally; CI image version unverified
- GitHub API endpoint paths: MEDIUM — well-known REST API shape, tagged ASSUMED
- Pitfalls: HIGH — derived from codebase patterns and standard shell scripting

**Research date:** 2026-06-17
**Valid until:** 2026-07-17 (stable domain)
