#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPT_FILE="${PROMPT_FILE:-${SCRIPT_DIR}/analyze-prompt.md}"
COMPARISON="${COMPARISON:-artifacts/benchmark-comparison.md}"
REPORT="${REPORT:-artifacts/benchmark-report.md}"

if [ ! -s "${COMPARISON}" ]; then
  echo "ERROR: ${COMPARISON} is missing or empty — run preprocess.sh first" >&2
  exit 1
fi

git fetch origin main --depth=50 2>/dev/null || true
PR_DIFF=$(git diff origin/main...HEAD -- '*.rs' '*.toml' 2>/dev/null | head -c 50000 || echo "(git diff unavailable)")

mkdir -p artifacts

export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"

CLAUDE_ALLOW_ROOT=1 claude --bare -p "$(printf 'Read %s using the Read tool, then write a benchmark analysis report to %s.\n\n<pr_diff>\n%s\n</pr_diff>' "${COMPARISON}" "${REPORT}" "${PR_DIFF}")" \
  --system-prompt-file "${PROMPT_FILE}" \
  --model anthropic/claude-sonnet-4-6 \
  --allowedTools "Read,Write" \
  --permission-mode bypassPermissions

if [ ! -s "${REPORT}" ]; then
  echo "ERROR: ${REPORT} is empty — Claude produced no output" >&2
  exit 1
fi

echo "${REPORT} generated ($(wc -l < "${REPORT}") lines)"
