#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Release Generate CHANGELOGs Script
# Writes and commits a CHANGELOG.md entry for every crate in the release set.
#
# Usage: ./release-generate-changelogs.sh --api-changes FILE
#
# Input is the audited release set from release-version-major-bumps.sh. Each crate
# takes one of four paths:
#
#   initial release   an existing CHANGELOG.md is left alone; otherwise a minimal
#                     "Initial release." file is created.
#   no commits, but a direct libdd-* dependency went major: a minimal entry listing
#                     the dependency bumps, formatted to match git-cliff's header so
#                     the file stays consistent.
#   no commits at all no entry; nothing to say.
#   commits           git-cliff, in two passes (see below).
#
# Every entry that is written is committed as "chore(release): update CHANGELOG.md
# for <crate>". Runs from the repository root, where cliff.toml lives.

set -euo pipefail

API_CHANGES=""
REMOTE_URL="https://github.com/datadog/libdatadog"

usage() {
    echo "Usage: $0 --api-changes FILE"
    echo ""
    echo "Options:"
    echo "  --api-changes FILE   Audited release set from release-version-major-bumps.sh (required)"
    echo "  --remote-url URL     Repository URL used in generated compare links"
    echo "                       (default: $REMOTE_URL)"
    echo "  --help, -h           Show this message"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-changes) API_CHANGES="${2:?--api-changes needs a value}"; shift 2 ;;
        --remote-url)  REMOTE_URL="${2:?--remote-url needs a value}"; shift 2 ;;
        --help|-h)     usage; exit 0 ;;
        *)             echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

[ -n "$API_CHANGES" ] || { echo "ERROR: --api-changes is required" >&2; exit 1; }
[ -f "$API_CHANGES" ] || { echo "ERROR: not a file: $API_CHANGES" >&2; exit 1; }
jq -e 'type == "array"' "$API_CHANGES" >/dev/null \
    || { echo "ERROR: $API_CHANGES is not a JSON array" >&2; exit 1; }

echo "Generating CHANGELOGS"

# Materialize the rows before looping. `done < <(jq ...)` would run jq in a process
# substitution, whose exit status no shell option reports: set -e and pipefail both
# ignore it, so a jq that dies mid-stream would leave the loop with no input and this
# script would exit 0 having written no CHANGELOG at all. The caller cannot catch that
# either: its no-changes-to-push guard sees the version-bump commits from the previous
# step and concludes there is something to release.
RELEASE_ROWS=$(jq -c '.[]' "$API_CHANGES")

while read -r bump; do
    # $ROWS is empty when there are no candidates; <<< still feeds one blank line.
    [ -n "$bump" ] || continue
    COMMITS=$(echo "$bump" | jq -r '.commits')
    RANGE=$(echo "$bump" | jq -r '.range')
    NAME=$(echo "$bump" | jq -r '.name')
    TAG=$(echo "$bump" | jq -r '.prev_tag')
    NEXT_TAG=$(echo "$bump" | jq -r '.tag')
    VERSION=$(echo "$bump" | jq -r '.version')
    CRATE_PATH=$(echo "$bump" | jq -r '.path')
    INITIAL_RELEASE=$(echo "$bump" | jq -r '.initial_release')
    MAJOR_BUMPS=$(echo "$bump" | jq -c '.major_bumps // []')

    if [ "$INITIAL_RELEASE" = "true" ]; then
        echo "Initial release for $NAME"

        # Use the existing CHANGELOG.md if present, otherwise create a minimal one
        if [ ! -f "$CRATE_PATH/CHANGELOG.md" ]; then
            echo "Creating CHANGELOG.md for $NAME..."
            RELEASE_DATE=$(date +%Y-%m-%d)
            printf '# Changelog\n\n\n## %s - %s\n\nInitial release.\n' "$VERSION" "$RELEASE_DATE" > "$CRATE_PATH/CHANGELOG.md"

            git add "$CRATE_PATH/CHANGELOG.md"
            git commit -m "chore(release): update CHANGELOG.md for $NAME"
        else
            echo "Using existing CHANGELOG.md for $NAME..."
        fi
        continue
    fi

    # FIXME: $COMMITS could be empty if there are no commits since last release
    if [ "$COMMITS" = "[]" ]; then
        if [ "$MAJOR_BUMPS" != "[]" ] && [ "$MAJOR_BUMPS" != "null" ]; then
            echo "No commits for $NAME but direct dependency major bumps; writing a minimal CHANGELOG entry"
            RELEASE_DATE=$(date +%Y-%m-%d)
            DEP_LINES=$(echo "$MAJOR_BUMPS" | jq -r '.[] | "- Bump `\(.dependency)` to a new major version (`\(.previous_req)` → `\(.current_req)`)"')

            # Match git-cliff's header (see cliff.toml): link the version to a compare view
            # against the previous tag when one exists.
            if [ -n "$TAG" ] && [ "$TAG" != "null" ]; then
                HEADER="## [$VERSION]($REMOTE_URL/compare/$TAG..$NEXT_TAG) - $RELEASE_DATE"
            else
                HEADER="## [$VERSION] - $RELEASE_DATE"
            fi

            ENTRY_FILE=$(mktemp "${TMPDIR:-/tmp}/changelog-entry-XXXXXX.md")
            printf '%s\n\n### Changed\n\n%s\n\n' "$HEADER" "$DEP_LINES" > "$ENTRY_FILE"

            if [ -f "$CRATE_PATH/CHANGELOG.md" ]; then
                # Insert the new section above the first existing release section (newest-first),
                # mirroring git-cliff --prepend placement and leaving the rest of the file intact.
                awk 'NR==FNR { e = e $0 ORS; next }
                     !inserted && /^## / { printf "%s", e; inserted=1 }
                     { print }
                     END { if (!inserted) printf "%s", e }' \
                  "$ENTRY_FILE" "$CRATE_PATH/CHANGELOG.md" > "$CRATE_PATH/CHANGELOG.md.tmp"
                mv "$CRATE_PATH/CHANGELOG.md.tmp" "$CRATE_PATH/CHANGELOG.md"
            else
                printf '# Changelog\n\n\n' > "$CRATE_PATH/CHANGELOG.md"
                cat "$ENTRY_FILE" >> "$CRATE_PATH/CHANGELOG.md"
            fi
            rm -f "$ENTRY_FILE"

            git add "$CRATE_PATH/CHANGELOG.md"
            git commit -m "chore(release): update CHANGELOG.md for $NAME"
        else
            echo "No commits since last release for $NAME, skipping CHANGELOG generation"
        fi
        continue
    fi

    # Build a tight range from commits already found by commits-since-release.sh.
    # This will save some time analising unnecessary commits and prevent unrelated commits
    # go through git-cliff filtering process.
    NEWEST_COMMIT=$(echo "$COMMITS" | jq -r '.[0].hash // empty')
    OLDEST_COMMIT=$(echo "$COMMITS" | jq -r '.[-1].hash // empty')
    # --verify matters: plain `git rev-parse <root-commit>^` exits non-zero but still
    # echoes "<sha>^" on stdout, so the `|| echo ""` fallback to $RANGE would never
    # fire and git-cliff would be handed a range start that does not resolve.
    OLDEST_PARENT=$(git rev-parse --verify "${OLDEST_COMMIT}^" 2>/dev/null || echo "")
    if [ -n "$OLDEST_PARENT" ] && [ -n "$NEWEST_COMMIT" ]; then
        COMMITS_RANGE="$OLDEST_PARENT..$NEWEST_COMMIT"
    else
        COMMITS_RANGE="$RANGE"
    fi
    echo "Executing git cliff for $NAME since $COMMITS_RANGE (oldest: $OLDEST_COMMIT, newest: $NEWEST_COMMIT), next tag: $NEXT_TAG..."

    # git-cliff's --include-path uses cumulative tree diffs rather than per-commit
    # diffs. This causes commits that don't touch the crate to pass the filter if an
    # earlier commit in the range does touch it. In order to avoid that a first pass
    # will generate the context inside the commit range and then a second step will
    # filter the the commits according to the previously computed range stored in COMMITS.
    CLIFF_CONTEXT_FILE=$(mktemp "${TMPDIR:-/tmp}/git-cliff-context-XXXXXX.json")
    CLIFF_HASHES_FILE=$(mktemp "${TMPDIR:-/tmp}/git-cliff-hashes-XXXXXX.json")
    CLIFF_FILTERED_FILE=$(mktemp "${TMPDIR:-/tmp}/git-cliff-filtered-XXXXXX.json")

    git cliff --context --tag "$NEXT_TAG" --ignore-tags ".*" -v "$COMMITS_RANGE" > "$CLIFF_CONTEXT_FILE"
    echo "$COMMITS" | jq '[.[].hash]' > "$CLIFF_HASHES_FILE"
    jq --slurpfile hashes "$CLIFF_HASHES_FILE" \
       --arg prev_tag "$TAG" \
       'map(. + {
         commits: [.commits[] | select(.id | IN($hashes[0][]))],
         previous: (.previous + {"version": $prev_tag})
       })' \
       "$CLIFF_CONTEXT_FILE" > "$CLIFF_FILTERED_FILE"
    git cliff --from-context "$CLIFF_FILTERED_FILE" -u -v --prepend "$CRATE_PATH/CHANGELOG.md"
    rm -f "$CLIFF_CONTEXT_FILE" "$CLIFF_HASHES_FILE" "$CLIFF_FILTERED_FILE"

    git add "$CRATE_PATH/CHANGELOG.md"
    git commit -m "chore(release): update CHANGELOG.md for $NAME"
done <<< "$RELEASE_ROWS"
