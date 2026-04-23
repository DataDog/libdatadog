#!/usr/bin/env bash
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

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Error: working directory is not clean. Commit or stash changes first." >&2
  git status --short >&2
  exit 1
fi

BRANCH="release/v${VERSION}"

echo "==> Fetching origin/main"
git fetch origin main

echo "==> Creating branch ${BRANCH} from origin/main"
git checkout -b "${BRANCH}" origin/main

echo "==> Bumping [workspace.package] version to ${VERSION} in Cargo.toml"
# Update only the `version = "..."` line inside the [workspace.package] section.
python3 - "$VERSION" <<'PY'
import re, sys, pathlib
version = sys.argv[1]
path = pathlib.Path("Cargo.toml")
text = path.read_text()

pattern = re.compile(
    r'(\[workspace\.package\][^\[]*?\nversion\s*=\s*")[^"]*(")',
    re.DOTALL,
)
new_text, n = pattern.subn(rf'\g<1>{version}\g<2>', text, count=1)
if n != 1:
    sys.exit("Error: could not locate version field in [workspace.package]")
path.write_text(new_text)
PY

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
