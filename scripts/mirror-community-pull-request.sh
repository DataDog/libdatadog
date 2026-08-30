#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -e

#
# Mirror an external (fork) pull request onto an internal branch so that CI
# runs with maintainer permissions rather than the restricted token available
# to fork PRs.
#
# The checked-out HEAD sha of the original PR is fetched directly from the
# fork over HTTPS (no persistent remote needed) and every commit is
# cherry-picked with GPG signing (-S) onto a fresh branch based on main.
# This avoids the "pwn request" problem: the workflow definitions and CI
# scripts that execute always come from the trusted base branch.
#
# Usage: mirror-community-pull-request.sh <pr-number> [<target-branch>]
#   <pr-number>:     GitHub PR number of the external/fork contribution.
#   <target-branch>: Branch to base the mirror on (default: main).
#
# The mirror branch is named: external/community-pr-<N>
#
# Prerequisites: gh (GitHub CLI), jq, git with GPG signing configured.

REPO="DataDog/libdatadog"
PR_NUMBER=$1
TARGET_BRANCH=${2:-main}
MIRROR_BRANCH="external/community-pr-${PR_NUMBER}"

#
# Check arguments.
#
if [ $# -eq 0 ]; then
    echo "Usage: $0 <pr-number> [<target-branch>]"
    echo "  <pr-number>:     PR number to mirror"
    echo "  <target-branch>: Base branch for the mirror (default: main)"
    echo ""
    echo "Creates branch 'external/community-pr-<N>' and a corresponding PR so CI"
    echo "runs with maintainer permissions on the fork's changes."
    exit 1
fi
if [ -z "$PR_NUMBER" ]; then
    echo "❌ PR number is not provided"
    exit 1
fi
if ! [[ "$PR_NUMBER" =~ ^[0-9]+$ ]]; then
    echo "❌ PR number must be numeric"
    exit 1
fi

#
# Check requirements.
#
echo "- Checking requirements"
gh --version 1>/dev/null 2>&1  || { echo "❌ gh is not installed. Please install GitHub CLI."; exit 1; }
gh auth status 1>/dev/null 2>&1 || { echo "❌ Not logged into GitHub CLI. Please run \`gh auth login\`."; exit 1; }
jq --version 1>/dev/null 2>&1  || { echo "❌ jq is not installed. Please install jq."; exit 1; }
git diff --quiet --exit-code   || { echo "❌ There are local changes. Please commit or stash them."; exit 1; }

#
# Fetch PR information.
#
echo "- Fetching PR #${PR_NUMBER} details"
PR_DATA=$(gh pr view "$PR_NUMBER" --repo "$REPO" \
    --json headRepository,headRepositoryOwner,headRefName,title,number,state,author \
    2>/dev/null || echo "")
if [ -z "$PR_DATA" ]; then
    echo "❌ PR #${PR_NUMBER} not found in ${REPO}"
    exit 1
fi

FORK_REPO=$(echo "$PR_DATA" | jq -r \
    '(.headRepository.nameWithOwner | select(. != "" and . != null))
     // (.headRepositoryOwner.login + "/" + .headRepository.name)
     // empty')
FORK_BRANCH=$(echo "$PR_DATA" | jq -r '.headRefName // empty')
PR_TITLE=$(echo "$PR_DATA"   | jq -r '.title  // empty')
PR_AUTHOR=$(echo "$PR_DATA"  | jq -r '.author.login // empty')
PR_LABELS=$(gh pr view "$PR_NUMBER" --repo "$REPO" --json labels \
    --jq '[.labels[].name] | join(",")')

if [ -z "$FORK_REPO" ] || [ -z "$FORK_BRANCH" ]; then
    echo "❌ Could not determine fork repository or branch for PR #${PR_NUMBER}"
    exit 1
fi

#
# Create mirror branch.
#
echo "- Mirroring PR #${PR_NUMBER} from ${FORK_REPO}:${FORK_BRANCH} → ${REPO}:${MIRROR_BRANCH}"

if git show-ref --verify --quiet "refs/heads/${MIRROR_BRANCH}" 2>/dev/null; then
    echo -n "Branch ${MIRROR_BRANCH} already exists locally. Delete and recreate? (y/n) "
    read -r ANSWER
    [ "$ANSWER" = "y" ] || { echo "Aborting."; exit 1; }
    git branch -D "$MIRROR_BRANCH"
fi

if git show-ref --verify --quiet "refs/remotes/origin/${MIRROR_BRANCH}" 2>/dev/null; then
    echo -n "Branch ${MIRROR_BRANCH} already exists on remote. Force-push over it? (y/n) "
    read -r ANSWER
    [ "$ANSWER" = "y" ] || { echo "Aborting."; exit 1; }
fi

# Fetch fork branch directly (no persistent remote required)
echo "- Fetching fork branch"
git fetch --quiet "https://github.com/${FORK_REPO}.git" "$FORK_BRANCH"

# Collect commit SHAs from the PR
echo "- Getting commits from PR"
PR_COMMITS=$(gh pr view "$PR_NUMBER" --repo "$REPO" --json commits \
    --jq '.commits[].oid')
if [ -z "$PR_COMMITS" ]; then
    echo "❌ No commits found in PR #${PR_NUMBER}"
    exit 1
fi

CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)

echo "- Creating ${MIRROR_BRANCH} from origin/${TARGET_BRANCH}"
git fetch --quiet origin "$TARGET_BRANCH"
git checkout -b "$MIRROR_BRANCH" "origin/${TARGET_BRANCH}"

#
# Cherry-pick and sign commits.
#
echo "- Cherry-picking and signing commits"
for COMMIT in $PR_COMMITS; do
    echo "  - ${COMMIT}"
    CHERRY_PICK_ARGS=("-S")
    PARENT_COUNT=$(git rev-list --parents -n 1 "$COMMIT" 2>/dev/null | wc -w)
    if [ "$PARENT_COUNT" -gt 2 ]; then
        CHERRY_PICK_ARGS+=("-m" "1")
    fi
    if ! git cherry-pick "${CHERRY_PICK_ARGS[@]}" "$COMMIT"; then
        if ! git diff --cached --quiet || ! git diff --quiet; then
            echo "❌ Failed to cherry-pick ${COMMIT} — resolve conflicts then re-run."
            git checkout "$CURRENT_BRANCH"
            exit 1
        else
            echo "    (empty commit, skipping)"
            git cherry-pick --skip
        fi
    fi
done

echo "- Pushing ${MIRROR_BRANCH} to origin"
git push -u origin "$MIRROR_BRANCH" --no-verify --force-with-lease

#
# Create mirror PR if one doesn't exist yet.
#
echo "- Checking for existing mirror PR"
EXISTING_PR=$(gh pr list --repo "$REPO" --head "$MIRROR_BRANCH" \
    --json number --jq '.[0].number // empty' 2>/dev/null)

if [ -n "$EXISTING_PR" ]; then
    MIRROR_PR_URL="https://github.com/${REPO}/pull/${EXISTING_PR}"
    echo "- Mirror PR already exists: #${EXISTING_PR}"
else
    echo "- Creating mirror PR"
    MIRROR_PR_BODY="This PR mirrors the changes from the external contribution below so that CI runs with maintainer permissions.

**Original PR:** https://github.com/${REPO}/pull/${PR_NUMBER}
**Original Author:** @${PR_AUTHOR}
**Original Branch:** ${FORK_REPO}:${FORK_BRANCH}

Closes #${PR_NUMBER}

---
*Mirror created by \`scripts/mirror-community-pull-request.sh\`.*"

    CREATE_ARGS=(
        --repo "$REPO"
        --base "$TARGET_BRANCH"
        --head "$MIRROR_BRANCH"
        --title "${PR_TITLE}"
        --body "$MIRROR_PR_BODY"
    )
    if [ -n "$PR_LABELS" ]; then
        CREATE_ARGS+=(--label "$PR_LABELS")
    fi

    MIRROR_PR_URL=$(gh pr create "${CREATE_ARGS[@]}" 2>/dev/null || true)
    if [ -z "$MIRROR_PR_URL" ]; then
        MIRROR_PR_URL="https://github.com/${REPO}/compare/${TARGET_BRANCH}...${MIRROR_BRANCH}"
        echo "- Could not create PR automatically; open one at: ${MIRROR_PR_URL}"
    fi
fi

echo ""
echo "✅ Done"
echo "   Original : https://github.com/${REPO}/pull/${PR_NUMBER} (@${PR_AUTHOR})"
echo "   Mirror   : ${MIRROR_PR_URL}"
echo "   Branch   : ${REPO}:${MIRROR_BRANCH}"

echo "- Restoring original branch"
git checkout "$CURRENT_BRANCH"
