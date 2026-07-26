#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPORT="${REPORT:-artifacts/benchmark-report.md}"
REPO="${REPO:-DataDog/libdatadog}"

PR_NUMBER="${CI_EXTERNAL_PULL_REQUEST_IID:-}"
if [ -z "${PR_NUMBER}" ]; then
  echo "No PR number found — skipping GitHub comment"
  exit 0
fi

if [ ! -s "${REPORT}" ]; then
  echo "ERROR: ${REPORT} is missing or empty — run analyze.sh first" >&2
  exit 1
fi

VERDICT_LINE=$(grep -m1 '^### Verdict' -A2 "${REPORT}" | tail -1 | tr -d '[:space:]' || true)
case "${VERDICT_LINE}" in
  pass) EMOJI="🟢" ;;
  warn) EMOJI="🟡" ;;
  fail) EMOJI="🔴" ;;
  *)    EMOJI="📊" ;;
esac

MARKER="<!-- bench-analysis-report -->"
REPORT_BODY=$(cat "${REPORT}")
COMMENT_BODY="${MARKER}
<details>
<summary>${EMOJI} Benchmark Analysis: ${VERDICT_LINE:-unknown}</summary>

${REPORT_BODY}
</details>"

COMMENT_ID=$(gh api "repos/${REPO}/issues/${PR_NUMBER}/comments" \
  --jq '.[] | select(.body | startswith("<!-- bench-analysis-report -->")) | .id' \
  | head -1)

if [ -n "${COMMENT_ID}" ]; then
  gh api --method PATCH \
    -H "Accept: application/vnd.github+json" \
    "repos/${REPO}/issues/comments/${COMMENT_ID}" \
    --field body="${COMMENT_BODY}"
  echo "Updated existing benchmark comment (id=${COMMENT_ID})"
else
  gh api --method POST \
    -H "Accept: application/vnd.github+json" \
    "repos/${REPO}/issues/${PR_NUMBER}/comments" \
    --field body="${COMMENT_BODY}"
  echo "Posted new benchmark comment on PR #${PR_NUMBER}"
fi

echo "report.sh done ($(wc -l < "${REPORT}") lines in report)"
