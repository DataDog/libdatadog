# CI tools

Rust helpers backing this repository's GitHub Actions. Each is published as a
release asset and downloaded, rather than compiled on every use.

## How a binary is resolved

`ci-tools-bin` is a composite action that each tool's action delegates to.
Given a package name it:

1. Reads the `[package]` version from `<tool>/Cargo.toml`.
2. Downloads the `<tool>` asset of the `ci-tool-<tool>-v<version>` release.
3. Falls back to `cargo build --release -p <tool>` when that release does not
   exist.

The version therefore comes from the commit being tested, and the fallback is
what keeps a pull request that modifies a tool gated by its own code: its new
version is not published yet, so the binary is built from source. Nothing needs
pinning in the workflows, and re-running an old workflow resolves the version
that commit recorded.

Tools are versioned and tagged independently, so changing one does not
republish the others. `ci-shared` is the exception: it is linked into every
tool, so a change there requires bumping all of them.

## Releasing a new version of a tool

1. Change the tool and bump its `[package]` version in `<tool>/Cargo.toml`.
2. Refresh the lockfile, which records the version of every member, and commit
   it:
   ```bash
   cargo update --workspace --offline --manifest-path .github/actions/Cargo.toml
   ```
3. Open the pull request. `ci-tools-version-guard` checks the bump and prints
   the tag command in its step summary. CI builds the tool from source here.
4. After merging, tag the merge commit. The tag ruleset covers every tag in
   this repository and requires a signature, so the tag has to be annotated
   and signed:
   ```bash
   tag=ci-tool-<tool>-v<version>
   git tag -s "$tag" -m "$tag" && git push origin "$tag"
   ```
   The ruleset also restricts who may create a tag, so this needs an account
   permitted to.
5. `ci-tools-release` checks the tag first: that it is well formed, that it
   names a binary package, that its version matches that package's manifest,
   and that its commit is on `main`. Only then does it wait on the
   `publish-ci-tools` environment, so an approval is given on a checked tag
   rather than on a name. The approval comes from `libdatadog-owners`, and
   whoever pushed the tag cannot give it. Once approved the binary is built
   and attached to a release of that tag, and later pull requests download it.

Skipping steps 4 and 5 is safe: every workflow keeps building from source
until the tag is released.

## Adding a new tool

Add the package to the `members` list in `Cargo.toml` with a `src/main.rs`, and
have its `action.yml` resolve the binary rather than build it:

```yaml
- name: Resolve <tool>
  id: bin
  uses: ./.github/actions/ci-tools-bin
  with:
    tool: <tool>
```

Run it as `"$BIN"`, with `BIN: ${{ steps.bin.outputs.path }}` in the step's
`env` rather than interpolated into the script. The action also outputs
`version` and `source` (`release` or `build`). The first release follows the
steps above; until it is tagged the tool builds from source.

## Worth knowing

- The guard only counts changes under `<tool>/src/`, `<tool>/Cargo.toml` and
  `<tool>/build.rs`. Editing an `action.yml` needs no bump.
- A version must increase, and must not name an already published tag.
- `ci-tools-release` refuses a tag whose commit is not reachable from `main`,
  since these binaries gate every pull request.
- Assets are x86_64 Linux only. Every consumer is a gate job on `ubuntu-latest`
  whose results reach the cross-platform matrices through `needs`, so no other
  platform ever needs one. A consumer on another runner builds from source
  instead of downloading a binary it could not run.
- Re-pushing an existing tag replaces the asset, so a version does not identify
  one fixed binary.
