#!/usr/bin/env bash
# Extract the vNext section from CHANGELOG.md
# Usage: ./extract_changelog_vnext.sh [CHANGELOG_FILE]

set -euo pipefail

CHANGELOG="${1:-CHANGELOG.md}"

if [ ! -f "$CHANGELOG" ]; then
  echo "Error: $CHANGELOG not found" >&2
  exit 1
fi

# Extract lines between "## vNext" and the next "##" header (or EOF)
awk '
  /^## vNext/ { in_vnext=1; next }
  /^##/ && in_vnext { exit }
  in_vnext { print }
' "$CHANGELOG" | sed 's/^[[:space:]]*$//' | sed '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba;}'

# Explanation:
# - Start capturing when we see "## vNext"
# - Stop when we see another "##" header
# - Remove leading/trailing blank lines

