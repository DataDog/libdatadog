#!/bin/bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Commits Since Release Script
# Takes JSON from publication-order.sh and finds commits since the last release tag for each crate
#
# Usage: ./commits-since-release.sh [OPTIONS] [JSON]
#
# Input: JSON from argument or stdin (output of publication-order.sh --format=json)
# Output: JSON with commits grouped by crate

set -euo pipefail

# Parse arguments
FORMAT="json"
VERBOSE=false
INPUT_JSON=""
# Default patterns to exclude (one per line, checked with grep -E)
EXCLUDE_PATTERNS="^Merge branch 
^Merge pull request "

for arg in "$@"; do
    case "$arg" in
        --format=*)
            FORMAT="${arg#--format=}"
            ;;
        --verbose|-v)
            VERBOSE=true
            ;;
        --exclude=*)
            # Add custom exclude pattern
            EXCLUDE_PATTERNS="${EXCLUDE_PATTERNS}
${arg#--exclude=}"
            ;;
        --no-exclude)
            # Disable default excludes
            EXCLUDE_PATTERNS=""
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS] [JSON]"
            echo ""
            echo "Takes JSON from publication-order.sh and finds commits since the last release tag for each crate."
            echo ""
            echo "Arguments:"
            echo "  JSON            JSON array of crates (if not provided, reads from stdin)"
            echo ""
            echo "Options:"
            echo "  --format=FORMAT   Output format: json (default), summary"
            echo "  --exclude=PATTERN Add a regex pattern to exclude commits by subject"
            echo "  --no-exclude      Disable default exclude patterns"
            echo "  --verbose, -v     Show verbose output to stderr"
            echo "  --help, -h        Show this help message"
            echo ""
            echo "Default excluded patterns:"
            echo "  - ^Merge branch "
            echo "  - ^Merge pull request "
            echo ""
            echo "Examples:"
            echo "  ./publication-order.sh --format=json libdd-common | ./commits-since-release.sh"
            echo "  ./commits-since-release.sh '[{\"name\":\"libdd-common\",\"version\":\"1.0.0\"}]'"
            echo "  ./commits-since-release.sh --format=summary \"\$(./publication-order.sh --format=json)\""
            echo "  ./commits-since-release.sh --exclude='^chore:' --exclude='^ci:' \"\$JSON\""
            echo ""
            echo "Output JSON format:"
            echo '  [{"name":"crate-name","version":"1.0.0","tag":"crate-name-v1.0.0","tag_exists":true,"commits":[...]}]'
            exit 0
            ;;
        -*)
            echo "Unknown option: $arg" >&2
            echo "Use --help for usage information" >&2
            exit 1
            ;;
        *)
            # Positional argument - treat as JSON input
            INPUT_JSON="$arg"
            ;;
    esac
done

# Read input JSON from stdin if not provided as argument
if [ -z "$INPUT_JSON" ]; then
    INPUT_JSON=$(cat)
fi

# Validate JSON
if ! echo "$INPUT_JSON" | jq empty 2>/dev/null; then
    echo "ERROR: Invalid JSON input" >&2
    exit 1
fi

# Get workspace root (for determining crate paths)
WORKSPACE_ROOT=$(cargo metadata --format-version=1 --no-deps 2>/dev/null | jq -r '.workspace_root' || pwd)

log_verbose() {
    if [ "$VERBOSE" = true ]; then
        echo "$@" >&2
    fi
}

# Check if a commit subject should be excluded
should_exclude() {
    local subject="$1"
    if [ -z "$EXCLUDE_PATTERNS" ]; then
        return 1  # Don't exclude
    fi
    
    # Check each pattern
    while IFS= read -r pattern; do
        if [ -n "$pattern" ] && echo "$subject" | grep -qE "$pattern"; then
            return 0  # Exclude
        fi
    done <<< "$EXCLUDE_PATTERNS"
    
    return 1  # Don't exclude
}

# Build output JSON
OUTPUT_JSON="["
FIRST=true

while read -r crate; do
    NAME=$(echo "$crate" | jq -r '.name')
    VERSION=$(echo "$crate" | jq -r '.version')
    TAG="${NAME}-v${VERSION}"
    
    log_verbose "Processing $NAME v$VERSION (tag: $TAG)..."
    
    # Find crate path from cargo metadata
    CRATE_PATH=$(cargo metadata --format-version=1 --no-deps 2>/dev/null | \
        jq -r --arg name "$NAME" '.packages[] | select(.name == $name) | .manifest_path' | \
        sed 's|/Cargo.toml$||' | \
        sed "s|^$WORKSPACE_ROOT/||")
    
    if [ -z "$CRATE_PATH" ]; then
        log_verbose "  WARNING: Could not find path for crate $NAME, using name as path"
        CRATE_PATH="$NAME"
    fi
    
    log_verbose "  Crate path: $CRATE_PATH"
    
    # Check if tag exists
    TAG_EXISTS=false
    COMMITS_JSON="[]"
    
    if git rev-parse "refs/tags/$TAG" >/dev/null 2>&1; then
        TAG_EXISTS=true
        log_verbose "  Tag exists, finding commits since $TAG..."
        
        # Check if tag is an ancestor of HEAD (i.e., release was merged back to main)
        # If not, use merge-base to find the common ancestor
        if git merge-base --is-ancestor "$TAG" HEAD 2>/dev/null; then
            COMMIT_RANGE="$TAG..HEAD"
            log_verbose "  Tag is ancestor of HEAD, using $COMMIT_RANGE"
        else
            MERGE_BASE=$(git merge-base "$TAG" HEAD 2>/dev/null || echo "")
            if [ -n "$MERGE_BASE" ]; then
                COMMIT_RANGE="$MERGE_BASE..HEAD"
                log_verbose "  Tag is NOT ancestor of HEAD, using merge-base: $COMMIT_RANGE"
            else
                log_verbose "  WARNING: Could not find merge-base, using $TAG..HEAD"
                COMMIT_RANGE="$TAG..HEAD"
            fi
        fi
        
        # Get commits since tag that affect this crate's directory
        # Format: hash|subject|author|date
        COMMITS_RAW=$(git log "$COMMIT_RANGE" --format="%H|%s|%an|%aI" -- "$CRATE_PATH" 2>/dev/null || true)
        
        if [ -n "$COMMITS_RAW" ]; then
            COMMITS_JSON="["
            COMMIT_FIRST=true
            
            while IFS='|' read -r hash subject author date; do
                if [ -n "$hash" ]; then
                    # Check if commit should be excluded
                    if should_exclude "$subject"; then
                        log_verbose "    Excluding: $subject"
                        continue
                    fi
                    
                    if [ "$COMMIT_FIRST" = true ]; then
                        COMMIT_FIRST=false
                    else
                        COMMITS_JSON+=","
                    fi
                    
                    # Escape special characters in subject for JSON
                    subject_escaped=$(echo "$subject" | jq -R .)
                    author_escaped=$(echo "$author" | jq -R .)
                    
                    COMMITS_JSON+="{\"hash\":\"$hash\",\"subject\":$subject_escaped,\"author\":$author_escaped,\"date\":\"$date\"}"
                fi
            done <<< "$COMMITS_RAW"
            
            COMMITS_JSON+="]"
        fi
        
        COMMIT_COUNT=$(echo "$COMMITS_JSON" | jq 'length')
        log_verbose "  Found $COMMIT_COUNT commits since $TAG"
    else
        log_verbose "  Tag does NOT exist - no previous release found"
    fi
    
    # Add to output
    if [ "$FIRST" = true ]; then
        FIRST=false
    else
        OUTPUT_JSON+=","
    fi
    
    OUTPUT_JSON+="{\"name\":\"$NAME\",\"version\":\"$VERSION\",\"path\":\"$CRATE_PATH\",\"tag\":\"$TAG\",\"tag_exists\":$TAG_EXISTS,\"commits\":$COMMITS_JSON}"
    
done < <(echo "$INPUT_JSON" | jq -c '.[]')

OUTPUT_JSON+="]"

# Ensure valid JSON output
OUTPUT_JSON=$(echo "$OUTPUT_JSON" | jq -c .)

# Output in requested format
case "$FORMAT" in
    json)
        echo "$OUTPUT_JSON"
        ;;
    
    summary)
        echo "Commits since last release by crate:"
        echo "========================================"
        echo "$OUTPUT_JSON" | jq -r '.[] | 
            "\(.name) v\(.version)" + 
            (if .tag_exists then 
                " (tag: \(.tag))\n  Commits: \(.commits | length)" +
                (if (.commits | length) > 0 then
                    "\n" + (.commits | map("    - \(.hash[0:8]) \(.subject)") | join("\n"))
                else "" end)
            else 
                "\n  No previous release tag found"
            end) + "\n"'
        ;;
    
    *)
        echo "Unknown format: $FORMAT" >&2
        echo "Available formats: json, summary" >&2
        exit 1
        ;;
esac
