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
^Merge pull request 
^chore\(release\)"

EXCLUDE_AUTHOR="dd-octo-sts\[bot\]"

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
            echo '  [{"name":"crate-name","version":"1.0.0","path":"crate-name","tag":"crate-name-v1.0.0",'
            echo '    "tag_exists":true,"tag_ancestor":"true","tag_commit":"<sha>",'
            echo '    "range":"<start-sha>..<head-sha>","commits":[...]}]'
            echo ""
            echo '  "range" is the commit range the listed commits were taken from, resolved to'
            echo '  SHAs: the tag commit, or the merge-base when the tag is not an ancestor of'
            echo '  HEAD, or the parent of the oldest commit found when there is no common'
            echo '  ancestor at all. Empty when the crate has no previous release tag.'
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

# Get cargo metadata once and cache it
METADATA=$(cargo metadata --format-version=1 --no-deps 2>/dev/null)

# Get workspace root (for determining crate paths)
WORKSPACE_ROOT=$(echo "$METADATA" | jq -r '.workspace_root' || pwd)

# Resolve HEAD once, so every crate's exported range ends at the same commit and callers
# can reuse the range later without re-resolving HEAD (which may have moved on by then).
HEAD_COMMIT=$(git rev-parse HEAD)

log_verbose() {
    if [ "$VERBOSE" = true ]; then
        echo "$@" >&2
    fi
}

# Check if a commit subject should be excluded
should_exclude() {
    local subject="$1"
    local author="$2"

    # Check if author should be excluded
    if echo "$author" | grep -qE "$EXCLUDE_AUTHOR"; then
        return 0  # Exclude
    fi

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
    
    # Find crate path from cached metadata
    CRATE_PATH=$(echo "$METADATA" | \
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
    TAG_ANCESTOR="unknown"
    TAG_COMMIT=""
    RANGE=""
    RANGE_START=""
    COMMITS_JSON="[]"

    if git rev-parse "refs/tags/$TAG" >/dev/null 2>&1; then
        TAG_EXISTS=true
        log_verbose "  Tag exists, finding commits since $TAG..."

        # Check if tag is an ancestor of HEAD (i.e., release was merged back to main)
        # If not, use merge-base to find the common ancestor.
        # Explicitly dereference annotated tags to their underlying commit: git merge-base does
        # not consistently dereference annotated tag objects across all git versions.
        #
        # RANGE_START is the same decision expressed as a commit SHA, and becomes the
        # exported `range`. Callers need a range they can hand to other git tooling, so
        # it is resolved to SHAs rather than left as a tag name.
        TAG_COMMIT=$(git rev-parse "${TAG}^{}" 2>/dev/null || echo "")
        if [ -z "$TAG_COMMIT" ]; then
            COMMIT_RANGE="$TAG..HEAD"
            TAG_ANCESTOR="no merge-base"
            log_verbose "  WARNING: Could not dereference tag $TAG to a commit, using $COMMIT_RANGE"
        elif git merge-base --is-ancestor "$TAG_COMMIT" HEAD 2>/dev/null; then
            COMMIT_RANGE="$TAG..HEAD"
            TAG_ANCESTOR="true"
            RANGE_START="$TAG_COMMIT"
            log_verbose "  Tag is ancestor of HEAD, using $COMMIT_RANGE"
        else
            MERGE_BASE=$(git merge-base "$TAG_COMMIT" HEAD 2>/dev/null || echo "")
            if [ -n "$MERGE_BASE" ]; then
                COMMIT_RANGE="$MERGE_BASE..HEAD"
                TAG_ANCESTOR="$MERGE_BASE"
                RANGE_START="$MERGE_BASE"
                log_verbose "  Tag is NOT ancestor of HEAD, using merge-base: $COMMIT_RANGE"
            else
                # Tag is on unrelated history. RANGE_START is derived from the commits
                # below, once we know which ones there are.
                COMMIT_RANGE="$TAG..HEAD"
                TAG_ANCESTOR="no merge-base"
                log_verbose "  WARNING: Could not find merge-base, using $TAG..HEAD"
            fi
        fi

        # Get commits since tag that affect this crate's directory
        # Use ASCII unit separator (0x1F) as delimiter - won't appear in commit messages
        COMMITS_JSON="["
        COMMIT_FIRST=true
        
        while IFS=$'\x1F' read -r hash subject author date; do
            if [ -n "$hash" ]; then
                # Check if commit should be excluded
                if should_exclude "$subject" "$author"; then
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
        done < <(git log "$COMMIT_RANGE" --format="%H%x1F%s%x1F%an%x1F%aI" -- "$CRATE_PATH" 2>/dev/null || true)
        
        COMMITS_JSON+="]"

        COMMIT_COUNT=$(echo "$COMMITS_JSON" | jq 'length')
        log_verbose "  Found $COMMIT_COUNT commits since $TAG"

        # No common ancestor with the tag: `$TAG..HEAD` spans HEAD's entire history, which
        # is fine for the path-filtered log above but far too wide to hand to git-cliff.
        # Start the exported range at the parent of the oldest commit we actually found,
        # so it covers those commits and nothing else.
        if [ -z "$RANGE_START" ] && [ -n "$TAG_COMMIT" ]; then
            OLDEST_COMMIT=$(echo "$COMMITS_JSON" | jq -r '.[-1].hash // empty')
            OLDEST_PARENT=""
            if [ -n "$OLDEST_COMMIT" ]; then
                # --verify matters: plain `git rev-parse <root-commit>^` exits non-zero but
                # still echoes "<sha>^" on stdout, so the `|| echo ""` fallback would never
                # fire and the range would start at a ref that does not resolve.
                OLDEST_PARENT=$(git rev-parse --verify "${OLDEST_COMMIT}^" 2>/dev/null || echo "")
            fi
            if [ -n "$OLDEST_PARENT" ]; then
                RANGE_START="$OLDEST_PARENT"
                log_verbose "  No common ancestor with $TAG: range starts at the parent of the oldest commit"
            else
                RANGE_START="$TAG_COMMIT"
                log_verbose "  WARNING: Could not derive a range start for $TAG, falling back to the tag commit"
            fi
        fi

        # Empty only when the tag could not be dereferenced at all.
        if [ -n "$RANGE_START" ]; then
            RANGE="${RANGE_START}..${HEAD_COMMIT}"
            log_verbose "  Range: $RANGE"
        fi
    else
        log_verbose "  Tag does NOT exist - no previous release found"
    fi
    
    # Add to output
    if [ "$FIRST" = true ]; then
        FIRST=false
    else
        OUTPUT_JSON+=","
    fi
    
    OUTPUT_JSON+="{\"name\":\"$NAME\",\"version\":\"$VERSION\",\"path\":\"$CRATE_PATH\",\"tag\":\"$TAG\",\"tag_exists\":$TAG_EXISTS,\"tag_ancestor\":\"$TAG_ANCESTOR\",\"tag_commit\":\"$TAG_COMMIT\",\"range\":\"$RANGE\",\"commits\":$COMMITS_JSON}"
    
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
                " (tag: \(.tag) ancestor: \(.tag_ancestor))\n  Commits: \(.commits | length)" +
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
