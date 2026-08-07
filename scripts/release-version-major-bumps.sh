#!/usr/bin/env bash

# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Release Version Major Bumps Script
# Audits every release candidate for direct libdd-* dependencies that went to a new
# major version, and promotes the crates that need it.
#
# Usage: ./release-version-major-bumps.sh --api-changes FILE --out FILE --branch BRANCH
#
# Input is the api-changes array produced by release-version-bumps.sh: the crates
# released there, plus the no-commit candidates carrying "pending_release": "true".
# Every row is audited the same way; only what happens to the result differs.
#
#   released, no bump          kept as it is
#   released, already major    kept as it is; nothing more to do
#   released, below major      promoted to major, version and tag updated
#   pending, earns a bump      promoted to major and pulled into the release
#   pending, earns nothing     dropped from the release entirely
#
# Diagnostics go to stdout (they are the caller's job log); the JSON result is written
# to --out. major-bumps-level.sh is resolved next to this script, so it always comes
# from the same checkout as the caller.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

API_CHANGES=""
OUT_FILE=""
BRANCH_NAME=""

usage() {
    echo "Usage: $0 --api-changes FILE --out FILE --branch BRANCH"
    echo ""
    echo "Options:"
    echo "  --api-changes FILE   Output of release-version-bumps.sh (required)"
    echo "  --out FILE           Where to write the audited JSON array (required)"
    echo "  --branch BRANCH      Branch cargo-release is allowed to operate on (required)"
    echo "  --help, -h           Show this message"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --api-changes) API_CHANGES="${2:?--api-changes needs a value}"; shift 2 ;;
        --out)         OUT_FILE="${2:?--out needs a value}"; shift 2 ;;
        --branch)      BRANCH_NAME="${2:?--branch needs a value}"; shift 2 ;;
        --help|-h)     usage; exit 0 ;;
        *)             echo "Unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

[ -n "$API_CHANGES" ] || { echo "ERROR: --api-changes is required" >&2; exit 1; }
[ -n "$OUT_FILE" ]    || { echo "ERROR: --out is required" >&2; exit 1; }
[ -n "$BRANCH_NAME" ] || { echo "ERROR: --branch is required" >&2; exit 1; }
[ -f "$API_CHANGES" ] || { echo "ERROR: not a file: $API_CHANGES" >&2; exit 1; }
jq -e 'type == "array"' "$API_CHANGES" >/dev/null \
    || { echo "ERROR: $API_CHANGES is not a JSON array" >&2; exit 1; }

AUDITED=$(mktemp "${TMPDIR:-/tmp}/api-changes-with-major-bumps-pre-commit.XXXXXX.json")
cleanup() { rm -f "$AUDITED"; }
trap cleanup EXIT

# Run the audit in a throwaway worktree so extra worktrees / cargo metadata do not touch
# the caller's checkout. Check it out at the proposal branch tip (HEAD) — the released ref
# plus this run's version bumps from the previous step. This is deliberate on both ends:
#   - It includes the dependency-requirement rewrites cargo-release made in the previous
#     step, so a dependency bumped to a new major IN THIS proposal propagates a major bump
#     to its dependents (e.g. protobuf 3->4 forces its dependents major).
#   - It is built from the released ref, NOT the workflow revision, so changes present only
#     on current main (and absent from a hotfix/older-ref release) never trigger a
#     spurious bump.
MAJOR_BUMPS_WT=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/major-bumps-wt.XXXXXX")
PROPOSAL_SHA=$(git rev-parse HEAD)

git worktree add --detach "$MAJOR_BUMPS_WT" "$PROPOSAL_SHA"
set +e
( cd "$MAJOR_BUMPS_WT" && "${SCRIPT_DIR}/major-bumps-level.sh" "$API_CHANGES" ) > "$AUDITED"
MB_RC=$?
git worktree remove --force "$MAJOR_BUMPS_WT" || true
set -e
if [[ "$MB_RC" -ne 0 ]]; then
    echo "Major bumps level script failed with code $MB_RC"
    echo "Major bumps level script output:"
    cat "$AUDITED"
    exit "$MB_RC"
fi

# Seed the result with every already-released crate. Pending crates are appended below
# only if they earn a major bump; those that do not stay out of the release entirely.
jq '[.[] | select(.pending_release != "true") | del(.pending_release)]' "$AUDITED" > "$OUT_FILE"

# iterate over the crates and, where a direct libdd-* dependency had a major bump, update the version
while read -r bump; do
    NAME=$(echo "$bump" | jq -r '.name')
    LEVEL=$(echo "$bump" | jq -r '.level')
    PREV_TAG=$(echo "$bump" | jq -r '.prev_tag')
    TAG=$(echo "$bump" | jq -r '.tag')
    VERSION=$(echo "$bump" | jq -r '.version')
    PENDING=$(echo "$bump" | jq -r '.pending_release // "false"')
    MAJOR_BUMPS=$(echo "$bump" | jq -c '.major_bumps')

    if [ "$MAJOR_BUMPS" = "[]" ]; then
        if [ "$PENDING" = "true" ]; then
            echo "No commits and no direct dependency major bumps for $NAME, keeping it out of the release"
        fi
        continue
    fi

    # A crate already bumped to major in the previous step needs nothing more. Pending
    # crates always have level "none" here, so this only short-circuits released crates.
    if [ "$LEVEL" = "major" ]; then
        echo "Skipping $NAME: already bumped at major level in the previous step (major_bumps: $MAJOR_BUMPS)"
        continue
    fi

    # Bump to major: either a pending (no-commit) crate whose direct dependency went major,
    # or a released crate bumped below major in the previous step. Both are handled the same.
    echo "Bumping $NAME to major due to direct dependency major bumps: $MAJOR_BUMPS"
    cargo release version -p "$NAME" --prev-tag-name "$PREV_TAG" --allow-branch "$BRANCH_NAME" -x major --no-confirm

    git commit -am "chore(release): update version for $NAME with major bumps"

    NEXT_VERSION=$(cargo metadata --format-version=1 --no-deps | jq -r --arg name "$NAME" '.packages[] | select(.name == $name) | .version')
    NEXT_TAG="$NAME-v$NEXT_VERSION"

    echo "Updating tag $TAG to $NEXT_TAG and version $VERSION to $NEXT_VERSION for $NAME"

    # Released crates are already in the result (seeded above): update them in place. Pending
    # crates are not: append them. The row is derived from the audit entry either way.
    ROW=$(echo "$bump" | jq --arg version "$NEXT_VERSION" --arg tag "$NEXT_TAG" \
        'del(.pending_release) | . + {level: "major", version: $version, tag: $tag}')
    jq --argjson row "$ROW" \
        'if any(.[]; .name == $row.name)
         then map(if .name == $row.name then $row else . end)
         else . + [$row] end' \
        "$OUT_FILE" > "${OUT_FILE}.tmp" \
        && mv "${OUT_FILE}.tmp" "$OUT_FILE"
done < <(jq -c '.[]' "$AUDITED")

# Output the results
echo "API changes with major bumps summary:"
jq . "$OUT_FILE"
