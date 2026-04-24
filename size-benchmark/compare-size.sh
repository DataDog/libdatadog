#!/usr/bin/env bash
# Compare binary size of the size-benchmark between two git refs.
#
# Usage:
#   ./size-benchmark/compare-size.sh --base <ref> --head <ref> [--output <file>]
#
# Output: markdown table printed to stdout (and optionally to --output file).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BASE_REF=""
HEAD_REF=""
OUTPUT_FILE=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --base)   BASE_REF="$2";    shift 2 ;;
        --head)   HEAD_REF="$2";    shift 2 ;;
        --output) OUTPUT_FILE="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

if [[ -z "$BASE_REF" || -z "$HEAD_REF" ]]; then
    echo "Usage: $0 --base <ref> --head <ref> [--output <file>]" >&2
    exit 1
fi

format_bytes() {
    local b=$1
    if   [[ $b -lt 1024 ]];           then echo "${b} B"
    elif [[ $b -lt $((1024*1024)) ]]; then printf "%.2f KB\n" "$(echo "scale=4; $b/1024"       | bc)"
    else                                   printf "%.2f MB\n" "$(echo "scale=4; $b/1024/1024"   | bc)"
    fi
}

# Build a ref in a temporary worktree, print byte count to stdout.
build_ref() {
    local ref="$1"
    local label="$2"
    local worktree
    worktree="$(mktemp -d)"

    echo "Building $label ($(git -C "$REPO_ROOT" rev-parse --short "$ref"))…" >&2

    git -C "$REPO_ROOT" worktree add --detach "$worktree" "$ref" 2>&1 | sed 's/^/  /' >&2

    # cargo writes to stderr; wc -c is the only stdout line.
    # Point CARGO_TARGET_DIR at the main worktree so both builds share the cache.
    # Redirect build stderr → our stderr so CI logs show progress.
    CARGO_TARGET_DIR="$REPO_ROOT/target" \
        bash "$worktree/size-benchmark/build-size-optimized.sh" 2>&3
    # (stdout = byte count, captured by the caller via $())

    git -C "$REPO_ROOT" worktree remove --force "$worktree" 2>/dev/null || true
    rm -rf "$worktree"
}

BASE_SHORT="$(git -C "$REPO_ROOT" rev-parse --short "$BASE_REF")"
HEAD_SHORT="$(git -C "$REPO_ROOT" rev-parse --short "$HEAD_REF")"

BASE_BYTES="$(build_ref "$BASE_REF" "base" 3>&2)"
HEAD_BYTES="$(build_ref "$HEAD_REF" "head" 3>&2)"

DIFF=$(( HEAD_BYTES - BASE_BYTES ))
DIFF_ABS=${DIFF#-}
[[ $DIFF -ge 0 ]] && SIGN="+" || SIGN="-"

PCT="$(echo "scale=2; $DIFF * 100 / $BASE_BYTES" | bc)"
PCT_ABS="$(echo "$PCT" | sed 's/^-//')"

BASE_FMT="$(format_bytes "$BASE_BYTES")"
HEAD_FMT="$(format_bytes "$HEAD_BYTES")"
DIFF_FMT="$(format_bytes "$DIFF_ABS")"

THRESHOLD=2
if   (( $(echo "$PCT < -$THRESHOLD" | bc -l) )); then EMOJI="🎉"  # significantly smaller
elif (( $(echo "$PCT < 0"           | bc -l) )); then EMOJI="✅"  # smaller, within noise
elif (( $(echo "$PCT == 0"          | bc -l) )); then EMOJI="➡️" # unchanged
elif (( $(echo "$PCT <= $THRESHOLD" | bc -l) )); then EMOJI="➡️" # larger, within noise
elif (( $(echo "$PCT <= 10"         | bc -l) )); then EMOJI="⚠️" # notable regression
else                                                   EMOJI="🚨"  # large regression
fi

TABLE="$(cat <<EOF
| | Size |
|---|---|
| Base (\`$BASE_SHORT\`) | $BASE_FMT |
| Head (\`$HEAD_SHORT\`) | $HEAD_FMT |
| Delta | ${SIGN}${DIFF_FMT} (${SIGN}${PCT_ABS}%) $EMOJI |
EOF
)"

echo "" >&2
echo "$TABLE"

if [[ -n "$OUTPUT_FILE" ]]; then
    echo "$TABLE" > "$OUTPUT_FILE"
    echo "Written to $OUTPUT_FILE" >&2
fi
