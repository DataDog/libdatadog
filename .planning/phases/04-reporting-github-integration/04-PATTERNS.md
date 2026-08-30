# Phase 4: Reporting & GitHub Integration - Pattern Map

**Mapped:** 2026-06-17
**Files analyzed:** 2 (1 new, 1 modified)
**Analogs found:** 2 / 2

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|-------------------|------|-----------|----------------|---------------|
| `.gitlab/bench-analysis/report.sh` | CI script | request-response (GitHub API) | `.gitlab/bench-analysis/analyze.sh` | exact |
| `.gitlab/bench-analysis.yml` | CI config | — | `.gitlab/bench-analysis.yml` (self) | exact |

---

## Pattern Assignments

### `.gitlab/bench-analysis/report.sh` (CI script, request-response)

**Analog:** `.gitlab/bench-analysis/analyze.sh`

**Shebang + strict mode** (analyze.sh lines 1-2):
```bash
#!/usr/bin/env bash
set -euo pipefail
```

**SCRIPT_DIR + env-var-overridable paths** (analyze.sh lines 4-7):
```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPT_FILE="${PROMPT_FILE:-${SCRIPT_DIR}/analyze-prompt.md}"
COMPARISON="${COMPARISON:-artifacts/benchmark-comparison.md}"
REPORT="${REPORT:-artifacts/benchmark-report.md}"
```
Apply to `report.sh` as:
```bash
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPORT="${REPORT:-artifacts/benchmark-report.md}"
REPO="${REPO:-DataDog/libdatadog}"
```

**Pre-condition guard on input file** (analyze.sh lines 9-12):
```bash
if [ ! -s "${COMPARISON}" ]; then
  echo "ERROR: ${COMPARISON} is missing or empty — run preprocess.sh first" >&2
  exit 1
fi
```
Apply to `report.sh` with `${REPORT}` instead of `${COMPARISON}`.

**Non-PR guard** — new pattern unique to report.sh (no analog, use this):
```bash
PR_NUMBER="${CI_EXTERNAL_PULL_REQUEST_IID:-}"
if [ -z "${PR_NUMBER}" ]; then
  echo "No PR number found — skipping GitHub comment"
  exit 0
fi
```
The `:-` default prevents `set -u` unbound variable error (Pitfall 4).

**Success echo with line count** (analyze.sh line 33):
```bash
echo "${REPORT} generated ($(wc -l < "${REPORT}") lines)"
```
Apply to `report.sh` as:
```bash
echo "report.sh done ($(wc -l < "${REPORT}") lines in report)"
```

**NVM sourcing block** (analyze.sh lines 19-20) — NOT needed in report.sh (no Claude invocation).

---

### `.gitlab/bench-analysis.yml` (CI config)

**Analog:** `.gitlab/bench-analysis.yml` (self, lines 34-35)

**Existing script block** (lines 34-35):
```yaml
    - bash .gitlab/bench-analysis/preprocess.sh
    - bash .gitlab/bench-analysis/analyze.sh
```
Add one line after `analyze.sh`:
```yaml
    - bash .gitlab/bench-analysis/report.sh
```
No other changes — artifacts block and GH_TOKEN export are already correct.

---

## Shared Patterns

### Verdict Extraction + Emoji Map
**Source:** RESEARCH.md Pattern 4 (no codebase analog — new pattern)
**Apply to:** `report.sh`
```bash
VERDICT_LINE=$(grep -m1 '^### Verdict' -A2 "${REPORT}" | tail -1 | tr -d '[:space:]' || true)
case "${VERDICT_LINE}" in
  pass) EMOJI="🟢" ;;
  warn) EMOJI="🟡" ;;
  fail) EMOJI="🔴" ;;
  *)    EMOJI="📊" ;;
esac
```
`|| true` prevents `set -e` failure when grep finds no match (Pitfall 5).

### Comment Body Construction
**Source:** RESEARCH.md Pattern 4
**Apply to:** `report.sh`
```bash
MARKER="<!-- bench-analysis-report -->"
REPORT_BODY=$(cat "${REPORT}")
COMMENT_BODY="${MARKER}
<details>
<summary>${EMOJI} Benchmark Analysis: ${VERDICT_LINE:-unknown}</summary>

${REPORT_BODY}
</details>"
```
Marker must be first line for reliable `startswith` matching in jq filter.

### gh api — Find Existing Comment
**Source:** RESEARCH.md Pattern 3
**Apply to:** `report.sh`
```bash
COMMENT_ID=$(
  gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --jq '.[] | select(.body | startswith("<!-- bench-analysis-report -->")) | .id' \
  | head -1
)
```
`head -1` guards against duplicate marker comments (take oldest). Returns empty string on no match.

### gh api — POST New Comment
**Source:** RESEARCH.md Pattern 1
**Apply to:** `report.sh` (when `COMMENT_ID` is empty)
```bash
gh api \
  --method POST \
  -H "Accept: application/vnd.github+json" \
  "repos/${REPO}/issues/${PR_NUMBER}/comments" \
  --field body="${COMMENT_BODY}"
```

### gh api — PATCH Existing Comment
**Source:** RESEARCH.md Pattern 2
**Apply to:** `report.sh` (when `COMMENT_ID` is non-empty)
```bash
gh api \
  --method PATCH \
  -H "Accept: application/vnd.github+json" \
  "repos/${REPO}/issues/comments/${COMMENT_ID}" \
  --field body="${COMMENT_BODY}"
```
Note: PATCH endpoint is `/issues/comments/{id}` — NOT `/issues/{pr}/comments/{id}` (Pitfall 2).

---

## Bats Test Pattern

### `.gitlab/bench-analysis/report.bats` (test)

**Analog:** `.gitlab/bench-analysis/preprocess.bats`

**File header + REPO_ROOT** (preprocess.bats lines 1-8):
```bash
#!/usr/bin/env bats
# <description>

REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/../.." && pwd)"
```

**CI-only skip guard** (preprocess.bats line 68):
```bash
command -v bp-analyzer >/dev/null || skip "bp-analyzer not available (CI-only)"
```
Apply to `report.bats` integration tests as:
```bash
[ -n "${GH_TOKEN:-}" ] || skip "GH_TOKEN not set (CI-only)"
```

**Static check pattern** (preprocess.bats lines 25-28):
```bash
@test "valid JSON: baseline.json and candidate.json parse without error" {
  python3 -c "import json; json.load(open('$BASELINE'))"
}
```
Apply to `report.bats` static checks:
```bash
@test "report.sh is syntactically valid" {
  bash -n "$REPO_ROOT/.gitlab/bench-analysis/report.sh"
}

@test "no-PR guard: script exits 0 and prints skip message when CI_EXTERNAL_PULL_REQUEST_IID is unset" {
  run env -u CI_EXTERNAL_PULL_REQUEST_IID bash "$REPO_ROOT/.gitlab/bench-analysis/report.sh"
  [ "$status" -eq 0 ]
  [[ "$output" == *"skipping GitHub comment"* ]]
}
```

---

## No Analog Found

No files in scope lack a close analog. All patterns are covered by `analyze.sh`, `preprocess.sh`, and `preprocess.bats`.

---

## Metadata

**Analog search scope:** `.gitlab/bench-analysis/`, `.gitlab/`
**Files scanned:** 4 (`analyze.sh`, `preprocess.sh`, `preprocess.bats`, `bench-analysis.yml`)
**Pattern extraction date:** 2026-06-17
