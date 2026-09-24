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

CLAUDE_BIN=$(which claude)

# claude refuses --dangerously-skip-permissions as root; run under a non-root user
CLAUDE_USER="claude-ci"
useradd -m "$CLAUDE_USER" 2>/dev/null || true
chmod o+x /root           # allow traversal into /root so claude-ci can reach nvm
chmod -R a+rX "$NVM_DIR"  # allow claude-ci to read/execute node and claude
chown -R "$CLAUDE_USER" artifacts/

# Write the prompt to a file to avoid quoting issues with PR_DIFF content
PROMPT_TMP=$(mktemp /tmp/claude-prompt.XXXXXX)
printf 'Read %s using the Read tool, then write a benchmark analysis report to %s.\n\n<pr_diff>\n%s\n</pr_diff>' \
  "${COMPARISON}" "${REPORT}" "${PR_DIFF}" > "$PROMPT_TMP"
chown "$CLAUDE_USER" "$PROMPT_TMP"

# Write the runner script using printf %q for safe shell quoting
RUNNER=$(mktemp /tmp/claude-run.XXXXXX.sh)
chmod 755 "$RUNNER"
{
  printf 'export ANTHROPIC_BASE_URL=%q\n'        "${ANTHROPIC_BASE_URL:-}"
  printf 'export ANTHROPIC_AUTH_TOKEN=%q\n'      "${ANTHROPIC_AUTH_TOKEN:-}"
  printf 'export ANTHROPIC_CUSTOM_HEADERS=%q\n'  "${ANTHROPIC_CUSTOM_HEADERS:-}"
  printf 'exec %q --bare -p "$(cat %q)" --system-prompt-file %q --model anthropic/claude-sonnet-4-6 --allowedTools "Read,Write" --dangerously-skip-permissions\n' \
    "$CLAUDE_BIN" "$PROMPT_TMP" "$PROMPT_FILE"
} > "$RUNNER"

su "$CLAUDE_USER" -s /bin/bash -c "bash '$RUNNER'"
rm -f "$RUNNER" "$PROMPT_TMP"

if [ ! -s "${REPORT}" ]; then
  echo "ERROR: ${REPORT} is empty — Claude produced no output" >&2
  exit 1
fi

echo "${REPORT} generated ($(wc -l < "${REPORT}") lines)"
