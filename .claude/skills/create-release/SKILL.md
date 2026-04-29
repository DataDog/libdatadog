---
name: create-release
description: Bump the Rust workspace version in root Cargo.toml, regenerate the lockfile, and open a draft PR on GitHub. Use this skill whenever the user says something like "create a release", "bump the version", "release vX.Y.Z", "prepare a release branch", or "bump workspace version". Trigger even if they just say "release X.Y.Z" or mention a semver version in a release context.
---

# Create Release

Automate the mechanical parts of cutting a release. The full flow (branch from `main`, bump `[workspace.package].version` in `Cargo.toml`, regenerate `Cargo.lock`, push, open draft PR) is implemented in `scripts/create-release.sh`. This skill's job is to collect the version, then invoke the script.

## Steps

### 1. Get the target version

If the user didn't supply a version already, ask: "What version should I bump to?"

The version must be bare semver (e.g. `32.0.0`, `31.1.0`) — no `v` prefix. The script re-validates and will reject bad input.

### 2. Run the script

```bash
scripts/create-release.sh <version>
```

The script will:

1. Fail fast if the working tree is dirty.
2. `git fetch origin main` and branch `release/v<version>` from `origin/main`.
3. Update only the `version` field in `[workspace.package]` of `Cargo.toml`.
4. Run `cargo update -w` to refresh workspace entries in `Cargo.lock`.
5. Commit (`chore: bump workspace version to <version>`), push with `-u`, and open a draft PR titled `chore: release v<version>` against `main`.

Return the PR URL from the `gh pr create` output to the user.

### 3. On failure

If the script exits non-zero, surface its error to the user and stop — do not try to finish the steps manually without checking in first.
