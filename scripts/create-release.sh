#!/usr/bin/env bash
# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

# Cut a release: branch from main, bump workspace version, regenerate lockfile,
# push, and open a draft PR.
#
# Usage: scripts/create-release.sh <version>
#   <version>  bare semver, e.g. 32.0.0 (no leading "v")

set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <version>" >&2
  exit 2
fi

VERSION="$1"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Error: '$VERSION' is not a valid bare semver (e.g. 32.0.0). Do not include a 'v' prefix." >&2
  exit 2
fi

if ! command -v gh >/dev/null 2>&1; then
  echo "Error: gh is not installed or could not be found in PATH." >&2
  exit 1
fi

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [[ -n "$(git status --porcelain --untracked-files=no)" ]]; then
  echo "Error: working directory has uncommitted tracked changes. Commit or stash them first." >&2
  git status --short --untracked-files=no >&2
  exit 1
fi

BRANCH="release/v${VERSION}"

echo "==> Fetching origin/main"
git fetch origin main

echo "==> Creating branch ${BRANCH} from origin/main"
git switch -c "${BRANCH}" origin/main

echo "==> Bumping [workspace.package] version to ${VERSION} in Cargo.toml"
sed -i.bak '/\[workspace\.package\]/,/^\[/{s/^version = ".*"/version = "'"${VERSION}"'"/;}' Cargo.toml
rm -f Cargo.toml.bak

echo "==> Regenerating lockfile (cargo update -w)"
cargo update -w

echo "==> Committing"
git add Cargo.toml Cargo.lock
git commit -m "chore: bump workspace version to ${VERSION}"

echo "==> Pushing ${BRANCH}"
git push -u origin "${BRANCH}"

echo "==> Creating draft PR"
gh pr create \
  --title "chore: release v${VERSION}" \
  --body "Bump workspace version to \`${VERSION}\` and regenerate lockfile." \
  --base main \
  --draft
