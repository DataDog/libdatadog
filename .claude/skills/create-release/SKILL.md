---
name: create-release
description: Bump the Rust workspace version in root Cargo.toml, regenerate the lockfile, and open a draft PR on GitHub. Use this skill whenever the user says something like "create a release", "bump the version", "release vX.Y.Z", "prepare a release branch", or "bump workspace version". Trigger even if they just say "release X.Y.Z" or mention a semver version in a release context.
---

# Create Release

Automate the mechanical parts of cutting a release: branch, version bump, lockfile update, draft PR.

## Steps

### 1. Get the target version

If the user didn't supply a version already, ask: "What version should I bump to?"

Validate it looks like a semver string (e.g. `32.0.0`, `31.1.0`). Do not add a `v` prefix in Cargo.toml — the file uses bare semver (e.g. `31.0.0`).

### 2. Check working directory is clean

```bash
git status --porcelain
```

If there are uncommitted changes, stop and tell the user to commit or stash them first.

### 3. Create and switch to release branch

Branch name: `release/v<version>` (e.g. `release/v32.0.0`). Always branch from `main`, regardless of the current branch.

```bash
git fetch origin main
git checkout -b release/v<version> origin/main
```

### 4. Bump the workspace version

Edit the root `Cargo.toml`. Find the `[workspace.package]` section and update the `version` field to the new value. Only change this one field — do not touch individual crate `Cargo.toml` files, `CHANGELOG.md`, or anything else.

Use the Edit tool to make a targeted replacement — do not rewrite the whole file.

### 5. Regenerate the lockfile

```bash
cargo build
```

This may take a moment. Wait for it to finish.

### 6. Commit

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump workspace version to <version>"
```

### 7. Push the branch

```bash
git push -u origin release/v<version>
```

### 8. Create draft PR

```bash
gh pr create \
  --title "chore: release v<version>" \
  --body "Bump workspace version to \`<version>\` and regenerate lockfile." \
  --base main \
  --draft
```

Return the PR URL to the user.
