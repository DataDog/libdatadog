#!/usr/bin/env bash

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -e

#
# Mirror an external (fork) pull request onto an internal branch so CI
# runs with maintainer permissions instead of the restricted token fork PRs
# get.
#
# The PR commits are fetched from the fork over HTTPS and cherry-picked
# with GPG signing (-S) onto a fresh branch based on main. No fork remote
# is added. The workflow definitions that CI runs come from the trusted
# base branch, not from the fork, which avoids the "pwn request" problem.
#
# Usage: mirror-community-pull-request.sh [--dry-run] [--no-push] <pr-number> [<target-branch>]
#   --dry-run:       Print what would be done and exit. The script still
#                    fetches the fork refs read-only to list the commits.
#                    Nothing is created, pushed, or labeled.
#   --no-push:       Create the mirror branch and cherry-pick locally, but
#                    skip the push and PR creation.
#   <pr-number>:     GitHub PR number of the external/fork contribution.
#   <target-branch>: Branch to base the mirror on (default: main).
#
# The mirror branch is named: external/community-pr-<N>
#
# Prerequisites: gh (GitHub CLI), jq, git with GPG signing configured.

REPO="DataDog/libdatadog"

#
# Parse arguments.
#
DRY_RUN=0
NO_PUSH=0
ARGS=()
for ARG in "$@"; do
    case "$ARG" in
        --dry-run) DRY_RUN=1 ;;
        --no-push) NO_PUSH=1 ;;
        -h|--help)
            echo "Usage: $0 [--dry-run] [--no-push] <pr-number> [<target-branch>]"
            echo "  --dry-run:       Print what would be done and exit. No changes are made."
            echo "  --no-push:       Create the mirror branch locally. Skip the push and PR creation."
            echo "  <pr-number>:     PR number to mirror"
            echo "  <target-branch>: Base branch for the mirror (default: main)"
            echo ""
            echo "Creates branch 'external/community-pr-<N>' and a corresponding PR so CI"
            echo "runs with maintainer permissions on the fork's changes."
            exit 0
            ;;
        *) ARGS+=("$ARG") ;;
    esac
done
PR_NUMBER=${ARGS[0]}
TARGET_BRANCH=${ARGS[1]:-main}
MIRROR_BRANCH="external/community-pr-${PR_NUMBER}"

#
# Check arguments.
#
if [ ${#ARGS[@]} -eq 0 ] || [ -z "$PR_NUMBER" ]; then
    echo "Usage: $0 [--dry-run] [--no-push] <pr-number> [<target-branch>]"
    echo "  --dry-run:       Print what would be done and exit. No changes are made."
    echo "  --no-push:       Create the mirror branch locally. Skip the push and PR creation."
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
if [ "$DRY_RUN" -eq 0 ]; then
    # Cherry-picks are signed with -S. Fail early so a signing failure is
    # not reported as a cherry-pick conflict.
    if [ -z "$(git config --get user.signingkey 2>/dev/null)" ] \
        && [ "$(git config --get commit.gpgsign 2>/dev/null)" != "true" ]; then
        echo "❌ Commit signing is not configured."
        echo "   Cherry-picks are signed with -S. Set user.signingkey or commit.gpgsign."
        exit 1
    fi
    if ! git diff --quiet --exit-code || ! git diff --cached --quiet --exit-code; then
        echo "❌ There are local changes. Please commit or stash them."
        exit 1
    fi
fi

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

#
# Add the 'external' label to a PR so external contributions can be told
# apart from internal ones. Non-fatal: the label may not exist in the
# repository yet.
#
add_external_label() {
    echo "- Adding 'external' label to ${2}"
    if ! gh pr edit "$1" --repo "$REPO" --add-label external >/dev/null 2>&1; then
        echo "- Could not add 'external' label to ${2}. Does it exist in ${REPO}?"
    fi
}

if [ -z "$FORK_REPO" ] || [ -z "$FORK_BRANCH" ]; then
    echo "❌ Could not determine fork repository or branch for PR #${PR_NUMBER}"
    exit 1
fi

#
# Create mirror branch.
#
echo "- Mirroring PR #${PR_NUMBER} from ${FORK_REPO}:${FORK_BRANCH} ➡️ ${REPO}:${MIRROR_BRANCH}"

# The overwrite prompts only apply to a real run. The dry-run report below
# flags existing branches instead.
CURRENT_BRANCH=$(git rev-parse --abbrev-ref HEAD)

# Read the remote directly. A local tracking ref stays behind forever once
# the remote branch is deleted, for example when a mirror PR is closed.
LS_REMOTE=$(git ls-remote origin "refs/heads/${MIRROR_BRANCH}" 2>&1) \
    || { echo "❌ Could not check origin for ${MIRROR_BRANCH}: ${LS_REMOTE}"; exit 1; }
REMOTE_SHA=$(echo "${LS_REMOTE}" | awk '{print $1}')

if [ "$DRY_RUN" -eq 0 ]; then
    if git show-ref --verify --quiet "refs/heads/${MIRROR_BRANCH}" 2>/dev/null; then
        echo -n "Branch ${MIRROR_BRANCH} already exists locally. Delete and recreate? (y/n) "
        read -r ANSWER || ANSWER=""
        [ "$ANSWER" = "y" ] || { echo "Aborting."; exit 1; }
        # git branch -D refuses to delete the branch this checkout is on.
        if [ "$CURRENT_BRANCH" = "$MIRROR_BRANCH" ]; then
            git checkout --detach --quiet
        fi
        git branch -D "$MIRROR_BRANCH"
    fi

    if [ -n "$REMOTE_SHA" ]; then
        echo -n "Branch ${MIRROR_BRANCH} exists on remote at ${REMOTE_SHA}. Force-push over it? (y/n) "
        read -r ANSWER || ANSWER=""
        [ "$ANSWER" = "y" ] || { echo "Aborting."; exit 1; }
    elif git show-ref --verify --quiet "refs/remotes/origin/${MIRROR_BRANCH}" 2>/dev/null; then
        # The tracking ref is stale because the remote branch is gone.
        # Remove it so it cannot mislead later checks or pushes.
        git update-ref -d "refs/remotes/origin/${MIRROR_BRANCH}"
    fi
fi

# Fetch the fork branch directly without adding a remote.
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

#
# Dry run: report what would be done and exit.
#
if [ "$DRY_RUN" -eq 1 ]; then
    echo ""
    echo "🔍 Dry run: nothing will be created, pushed, or cherry-picked"
    echo "   PR     : #${PR_NUMBER} ${PR_TITLE} (@${PR_AUTHOR})"
    echo "   Fork   : ${FORK_REPO}:${FORK_BRANCH}"
    echo "   Target : origin/${TARGET_BRANCH}"
    echo "   Mirror : ${REPO}:${MIRROR_BRANCH}"
    echo "   Commits:"
    for COMMIT in $PR_COMMITS; do
        SUBJECT=$(git log -1 --format='%h %s' "$COMMIT" 2>/dev/null \
            || echo "${COMMIT} (not found locally)")
        echo "     - ${SUBJECT}"
    done
    if git show-ref --verify --quiet "refs/heads/${MIRROR_BRANCH}" 2>/dev/null; then
        echo "   🚧 Local branch ${MIRROR_BRANCH} exists; it would be deleted and recreated."
    fi
    if [ -n "${REMOTE_SHA}" ]; then
        echo "   🚧 Remote branch ${MIRROR_BRANCH} exists at ${REMOTE_SHA}; it would be force-pushed over."
    fi
    for COMMIT in $PR_COMMITS; do
        if [ -n "$(git diff-tree --root --no-commit-id --name-only -r -m --first-parent \
                "$COMMIT" -- .github/ 2>/dev/null)" ]; then
            echo "   🚧 Commit ${COMMIT} touches .github/ (CI workflow definitions)."
        fi
    done
    echo ""
    echo "Run again without --dry-run to create the mirror branch and PR."
    exit 0
fi

#
# Label the original PR as external. Dry-run exits above, so this only
# runs when the mirror is being built.
#
add_external_label "${PR_NUMBER}" "original PR #${PR_NUMBER}"

# Create the mirror branch without tracking origin/<target>. On git
# >= 2.56 a tracked upstream makes 'git push origin <branch>' update the
# upstream, so the push would land on main.
echo "- Creating ${MIRROR_BRANCH} from origin/${TARGET_BRANCH}"
git fetch --quiet origin "$TARGET_BRANCH"
git checkout --no-track -b "$MIRROR_BRANCH" "origin/${TARGET_BRANCH}"

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
            echo "❌ Failed to cherry-pick ${COMMIT}."
            # Roll back the in-progress pick. A plain checkout would fail
            # on the unmerged index and leave sequencer state behind.
            git cherry-pick --abort 2>/dev/null \
                || git cherry-pick --quit 2>/dev/null \
                || true
            git checkout "$CURRENT_BRANCH"
            echo "   Kept branch ${MIRROR_BRANCH} with the commits picked so far."
            echo "   Re-run the script to delete and rebuild it."
            exit 1
        else
            echo "    (empty commit, skipping)"
            git cherry-pick --skip
        fi
    fi
done

#
# Refuse to push a mirror that changes CI definitions. A pushed branch runs
# 'on: push' workflows at the pushed commit, so a contributor-modified
# workflow file would run with the repository's credentials before review.
#
TOUCHED_CI=$(git diff "origin/${TARGET_BRANCH}" --name-only -- .github/)
if [ -n "$TOUCHED_CI" ]; then
    echo "❌ This PR modifies CI definitions under .github/:"
    echo "   ${TOUCHED_CI//$'\n'/$'\n   '}"
    if [ "$NO_PUSH" -eq 1 ]; then
        echo "   --no-push skips the push. Review these files before pushing manually."
    else
        echo "   Pushing it would run contributor-controlled workflows."
        echo "   Review the files. If they are safe, push the mirror branch by hand:"
        echo "   git push -u origin ${MIRROR_BRANCH}:${MIRROR_BRANCH} --force-with-lease=refs/heads/${MIRROR_BRANCH}:${REMOTE_SHA}"
        git checkout "$CURRENT_BRANCH"
        exit 1
    fi
fi

if [ "$NO_PUSH" -eq 1 ]; then
    echo "- Skipping push and PR creation (--no-push)"
    echo "  Push manually with: git push -u origin ${MIRROR_BRANCH}:${MIRROR_BRANCH} --force-with-lease=refs/heads/${MIRROR_BRANCH}:${REMOTE_SHA}"
else
    # Push with an explicit src:dst refspec. On git >= 2.56 a bare
    # 'git push origin <branch>' follows the branch upstream and would
    # update the target branch instead of the mirror branch.
    # Pin the lease to the SHA observed at consent time. An empty value
    # requires the remote branch to not exist, so a branch created in the
    # meantime also fails the push.
    echo "- Pushing ${MIRROR_BRANCH} to origin"
    if ! git push -u origin "${MIRROR_BRANCH}:${MIRROR_BRANCH}" \
            --force-with-lease="refs/heads/${MIRROR_BRANCH}:${REMOTE_SHA}"; then
        echo "❌ Push failed. ${MIRROR_BRANCH} on origin changed since the force-push consent."
        echo "   Run the script again to re-check the remote and consent."
        git checkout "$CURRENT_BRANCH"
        exit 1
    fi

    #
    # Create mirror PR if one doesn't exist yet.
    #
    echo "- Checking for existing mirror PR"
    EXISTING_PR=$(gh pr list --repo "$REPO" --head "$MIRROR_BRANCH" \
        --json number --jq '.[0].number // empty' 2>/dev/null)

    if [ -n "$EXISTING_PR" ]; then
        MIRROR_PR_URL="https://github.com/${REPO}/pull/${EXISTING_PR}"
        echo "- Mirror PR already exists: #${EXISTING_PR}"
        add_external_label "${MIRROR_BRANCH}" "mirror PR"
    else
        echo "- Creating mirror PR"
        MIRROR_PR_BODY="This PR mirrors the changes from the external contribution linked below so CI runs with maintainer permissions.

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
            --title "${PR_TITLE} 🪩 #${PR_NUMBER}"
            --body "$MIRROR_PR_BODY"
        )
        if [ -n "$PR_LABELS" ]; then
            CREATE_ARGS+=(--label "$PR_LABELS")
        fi

        CREATE_ERR=$(mktemp)
        if MIRROR_PR_URL=$(gh pr create "${CREATE_ARGS[@]}" 2>"$CREATE_ERR"); then
            rm -f "$CREATE_ERR"
        else
            echo "- PR creation failed:"
            sed 's/^/    /' "$CREATE_ERR"
            rm -f "$CREATE_ERR"
            MIRROR_PR_URL=""
        fi
        if [ -n "$MIRROR_PR_URL" ]; then
            add_external_label "${MIRROR_BRANCH}" "mirror PR"
        else
            MIRROR_PR_URL="https://github.com/${REPO}/compare/${TARGET_BRANCH}...${MIRROR_BRANCH}"
            echo "- Could not create the PR automatically. Open one at ${MIRROR_PR_URL}"
        fi
    fi
fi

echo ""
echo "✅ Done"
echo "   Original : https://github.com/${REPO}/pull/${PR_NUMBER} (@${PR_AUTHOR})"
if [ "$NO_PUSH" -eq 1 ]; then
    echo "   Branch   : ${MIRROR_BRANCH} (local only; not pushed)"
else
    echo "   Mirror   : ${MIRROR_PR_URL}"
    echo "   Branch   : ${REPO}:${MIRROR_BRANCH}"
fi

echo "- Restoring original branch"
git checkout "$CURRENT_BRANCH"
