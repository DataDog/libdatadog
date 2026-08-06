#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Release Version Bumps Script
# Turns the output of commits-since-release.sh into api-changes.json, running
# `cargo release version` for each crate that is actually being released.
#
# Usage: ./release-version-bumps.sh --commits-by-crate FILE --out FILE --branch BRANCH
#                                   [--hotfix] [--bypass-standard-checks]
#
# For every crate in the input, one of four things happens:
#
#   deferred  no commits of its own but a tag exists -- recorded with
#             "pending_release": "true" and level "none", so the caller's libdd-*
#             major-bump check can pull it back into the release, or drop it.
#   skipped   its tag is not the latest for that crate, so a newer release already
#             exists elsewhere. Overridden by --hotfix and --bypass-standard-checks.
#   released  semver-level.sh picks the level, cargo-release applies it.
#   initial   no tag at all: released at 0.1.0, or the run fails.
#
# Diagnostics go to stdout (they are the caller's job log); the JSON result is written
# to --out. semver-level.sh is resolved next to this script, so it always comes from
# the same checkout as the caller.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

COMMITS_BY_CRATE=""
OUT_FILE=""
BRANCH_NAME=""
IS_HOTFIX=false
BYPASS_STANDARD_CHECKS=false

usage() {
    echo "Usage: $0 --commits-by-crate FILE --out FILE --branch BRANCH [--hotfix] [--bypass-standard-checks]"
    echo ""
    echo "Options:"
    echo "  --commits-by-crate FILE   Output of commits-since-release.sh (required)"
    echo "  --out FILE                Where to write the api-changes JSON array (required)"
    echo "  --branch BRANCH           Branch cargo-release is allowed to operate on (required)"
    echo "  --hotfix                  Release even when the crate's tag is not the latest"
    echo "  --bypass-standard-checks  Same, for testing runs"
    echo "  --help, -h                Show this message"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --commits-by-crate) COMMITS_BY_CRATE="${2:?--commits-by-crate needs a value}"; shift 2 ;;
        --out)              OUT_FILE="${2:?--out needs a value}"; shift 2 ;;
        --branch)           BRANCH_NAME="${2:?--branch needs a value}"; shift 2 ;;
        --hotfix)                 IS_HOTFIX=true; shift ;;
        --bypass-standard-checks) BYPASS_STANDARD_CHECKS=true; shift ;;
        --help|-h)          usage; exit 0 ;;
        *)                  echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

[ -n "$COMMITS_BY_CRATE" ] || { echo "ERROR: --commits-by-crate is required" >&2; exit 1; }
[ -n "$OUT_FILE" ]         || { echo "ERROR: --out is required" >&2; exit 1; }
[ -n "$BRANCH_NAME" ]      || { echo "ERROR: --branch is required" >&2; exit 1; }
[ -f "$COMMITS_BY_CRATE" ] || { echo "ERROR: not a file: $COMMITS_BY_CRATE" >&2; exit 1; }
jq -e 'type == "array"' "$COMMITS_BY_CRATE" >/dev/null \
    || { echo "ERROR: $COMMITS_BY_CRATE is not a JSON array" >&2; exit 1; }

echo "Release version bumps..."

# Initialize results array. It holds one row per candidate crate: those released here,
# and those with no commits of their own, which carry "pending_release": "true" and are
# only released if the libdd-* major-bump check pulls them back in.
echo "[]" > "$OUT_FILE"

append_row() {
    # append_row JQ_ARGS... -- reads $OUT_FILE, appends, writes back.
    local tmp="${OUT_FILE}.tmp"
    jq "$@" "$OUT_FILE" > "$tmp" && mv "$tmp" "$OUT_FILE"
}

# iterate over the commits and execute cargo release for each crate
while read -r crate; do
    NAME=$(echo "$crate" | jq -r '.name')
    TAG=$(echo "$crate" | jq -r '.tag')
    TAG_PREFIX="$NAME-v"
    CRATE_PATH=$(echo "$crate" | jq -r '.path')
    TAG_EXISTS=$(echo "$crate" | jq -r '.tag_exists')
    COMMITS=$(echo "$crate" | jq -r '.commits')
    INITIAL_RELEASE=false
    TAG_COMMIT=""
    RANGE=""
    LEVEL=""

    # if there are no commits and there is an existing tag, do not release the crate here.
    # but record it as a pending candidate
    if [ "$COMMITS" = "[]" ] && [ "$TAG_EXISTS" = "true" ]; then
        VERSION=$(echo "$crate" | jq -r '.version')
        echo "No commits since last release for $NAME; deferring to the libdd-* major-bump check"
        append_row --arg name "$NAME" \
            --arg tag "$TAG" \
            --arg version "$VERSION" \
            --arg path "$CRATE_PATH" \
            '. += [{"name": $name, "level": "none", "tag": $tag, "prev_tag": $tag, "version": $version, "range": "", "commits": [], "path": $path, "initial_release": "false", "pending_release": "true"}]'
        continue
    fi

    if [ "$TAG_EXISTS" = "true" ]; then
        TAG_COMMIT=$(echo "$crate" | jq -r '.tag_commit')
        RANGE=$(echo "$crate" | jq -r '.range')
        if [ -z "$TAG_COMMIT" ] || [ -z "$RANGE" ]; then
            echo "ERROR: Could not dereference tag $TAG to a commit" >&2
            exit 1
        fi
        echo "Using $RANGE as range (tag: $TAG)"

        if [ "$(echo "$crate" | jq -r '.tag_in_local_branch')" != "true" ]; then
            echo "Warning: tag $TAG (commit $TAG_COMMIT) is not in any local branch (normal for squash-merged releases)"
        fi

        # if there is a tag more recent than $TAG, continue the loop.
        LATEST_TAG=$(echo "$crate" | jq -r '.latest_tag')
        if [ "$LATEST_TAG" != "$TAG" ]; then
            echo "Tag $TAG is not the latest. Latest is: $LATEST_TAG. main branch has the latest release for $NAME"

            # do not skip the release for hotfix branches
            if [ "$IS_HOTFIX" = "true" ]; then
                echo "Continuing with the release for $NAME because it is a hotfix"
            else
                if [ "$BYPASS_STANDARD_CHECKS" = "false" ]; then
                    echo "Skipping release for $NAME"
                    continue
                else
                    echo "Continuing with the release for $NAME because bypass_standard_checks is true"
                fi
            fi
        fi

        echo "Executing semver-level.sh for $NAME since $RANGE (tag: $TAG)..."
        # stderr is folded in so the reason travels with a failure; without this the
        # capture swallows it and the run aborts with nothing to go on.
        if ! SEMVER_LEVEL=$("${SCRIPT_DIR}/semver-level.sh" "$NAME" "refs/tags/$TAG" 2>&1); then
            echo "ERROR: semver-level.sh failed for $NAME:" >&2
            echo "$SEMVER_LEVEL" >&2
            exit 1
        fi
        echo "Semver level: $SEMVER_LEVEL"

        LEVEL=$(echo "$SEMVER_LEVEL" | jq -r '.level')

        echo "Executing cargo release for $NAME since $TAG with level $LEVEL..."
        cargo release version -p "$NAME" --prev-tag-name "$TAG" --allow-branch "$BRANCH_NAME" -x "$LEVEL" --no-confirm

    else
        echo "No previous release tag for $NAME, preparing initial release..."

        # Use the version from the crate metadata
        VERSION=$(echo "$crate" | jq -r '.version')
        LEVEL="major"
        TAG=""
        RANGE=""

        # fail when the version is not an initial release
        if [ "$VERSION" != "0.1.0" ]; then
            echo "Error: $NAME is not a 0.1.0 release" >&2
            exit 1
        fi

        INITIAL_RELEASE=true

        echo "Executing cargo release for $NAME with level $LEVEL..."
        cargo release version -p "$NAME" --allow-branch "$BRANCH_NAME" -x "$LEVEL" --no-confirm
    fi

    # Commit the changes
    cargo release commit --no-confirm -x

    NEXT_VERSION=$(cargo metadata --format-version=1 --no-deps | jq -r --arg name "$NAME" '.packages[] | select(.name == $name) | .version')
    NEXT_TAG="$TAG_PREFIX$NEXT_VERSION"

    # Add to results array
    append_row --arg name "$NAME" \
        --arg level "$LEVEL" \
        --arg tag "$NEXT_TAG" \
        --arg prev_tag "$TAG" \
        --arg version "$NEXT_VERSION" \
        --arg range "$RANGE" \
        --argjson commits "$COMMITS" \
        --arg path "$CRATE_PATH" \
        --arg initial_release "$INITIAL_RELEASE" \
        '. += [{"name": $name, "level": $level, "tag": $tag, "prev_tag": $prev_tag, "version": $version, "range": $range, "commits": $commits, "path": $path, "initial_release": $initial_release}]'
done < <(jq -c '.[]' "$COMMITS_BY_CRATE")

# Output the results
echo "API changes summary:"
jq . "$OUT_FILE"
