# Release proposals (publishing crates to crates.io)

Practical guide to the flow that publishes `libdd-*` (and other workspace) crates to
crates.io.

> **Not this flow:** the FFI **artifact** release (`vX.Y.Z` tarballs + headers) is a
> different pipeline — `scripts/create-release.sh` and libddprof-build's
> `draft_github_release.sh`. It uses `release/vX.Y.Z` branches (one path segment), which
> do not collide with the `release/<crate>/<timestamp>` branches used here.

## At a glance

```
  ┌─ GitHub Actions ────────────────────────────────────────────────┐
  │ 1. workflow_dispatch: "Release - Open a release proposal PR"    │
  │      release-proposal-dispatch.yml                              │
  │    ├─ validate crates, membership, no ongoing proposal          │
  │    ├─ create ephemeral branch  release/<crates>/<timestamp>     │
  │    ├─ create proposal branch   release-proposal/<crates>/<ts>   │
  │    ├─ per crate: semver-level.sh → cargo release version       │
  │    ├─ force major on direct libdd-* major bumps                 │
  │    ├─ git-cliff CHANGELOGs                                      │
  │    └─ draft PR: release-proposal/... → release/...              │
  │ 2. release-proposal-test.yml (on that push / PR)                │
  │      cargo package + compile dd-trace-rs against the packages   │
  └─────────────────────────────────────────────────────────────────┘
                              │ squash-merge the proposal PR
                              ▼  (push to release/**)
  ┌─ GitLab (libddprof-build, via the ddbuild mirror) ──────────────┐
  │ 3. publish_cargo_crates  (MANUAL job, DRY_RUN=true by default)  │
  │    ├─ crates-to-package.sh → publication-order.sh → tags        │
  │    ├─ publish-crates.sh: test, cargo publish, crates.io owners  │
  │    ├─ create annotated GitHub tags <crate>-v<version>           │
  │    └─ create_pr_to_merge_release_branch.sh: release/... → main  │
  └─────────────────────────────────────────────────────────────────┘
                              │ merge that PR
                              ▼
                    main has the bumps + CHANGELOGs
```

## 1. Open the proposal

Actions → **Release - Open a release proposal PR** → Run workflow.

| Input | Notes |
|---|---|
| `crates` | Comma-separated. Each crate is released together with its workspace `libdd-*` dependencies (`scripts/publication-order.sh`). Only publishable crates (`publish != false`) are accepted. |
| `main_start_ref` | Empty = tip of `origin/main`. A SHA/branch/tag is allowed **only if reachable from `origin/main`** or from the matching `origin/hotfix/<crate>/N.x.x`. `refs/pull/*` is rejected. |
| `bypass_standard_checks` | Testing only: skips the ongoing-proposal guard and the team-membership check, uses `release-testing/` + `release-proposal-testing/` prefixes, pushes plainly (no verified commits), and stops skipping crates whose tag is not the latest. |

Guards that will stop you:

- **Ongoing proposal** — any existing `origin/release-proposal/*` or `origin/release/*/*`
  branch aborts the run. One release at a time.
- **Membership** — the actor must be in `Datadog/apm-common-components-core`.
- **Untrusted `cargo-release` config** — the tree must not mention
  `pre-release-hook` / `pre-release-replacements` anywhere in `Cargo.toml` / `release.toml`.
- Release scripts are always taken from the **workflow revision** (`github.sha`), not from
  `main_start_ref`.

What the job produces: two branches, one bump commit + one CHANGELOG commit per crate
(pushed via `DataDog/commit-headless` so they are verified), and a **draft** PR titled
`chore(release): proposal for <crates>`, based on the ephemeral `release/...` branch.
The `release-dispatch-data` artifact (1 day retention) holds the intermediate JSON
(`commits-by-crate.json`, `api-changes*.json`) — start debugging there.

### Which crates actually get released

`scripts/commits-since-release.sh` lists commits since `<crate>-v<version>` **that touch
the crate's directory**, dropping merge commits, `chore(release)` subjects, and anything
authored by `dd-octo-sts[bot]`.

- Commits found → the crate is bumped.
- No commits and a tag exists → deferred; released **only** if a direct `libdd-*`
  dependency goes major in this proposal.
- No tag at all → initial release, forced to `major`, and the run **fails unless the
  manifest version is exactly `0.1.0`**.
- The crate's resolved tag is not the latest SemVer tag for that crate → skipped
  (that release is already on `main`), unless it is a hotfix or `bypass_standard_checks`.

## 2. How the bump level is decided

`scripts/semver-level.sh <crate> refs/tags/<prev-tag>` computes `major | minor | patch`
from two tools and takes the **higher** of the two:

1. `cargo semver-checks -p <crate> --all-features --baseline-rev <tag>`
   → `major` on "requires new major", `minor` on "requires new minor", `minor` if the
   crate is absent from the baseline (new crate).
2. `cargo public-api --package <crate> diff <tag>..HEAD` (skipped if 1. already said
   major, or the crate is new)
   → removed items = `major`; changed items = `major` **if** a difference survives
   normalization (diff markers, `#[...]` attributes and `const`/`async`/`unsafe` are
   stripped); added items = `minor`.

No signal at all ⇒ `patch`. The level is then fed to `cargo release version -p <crate> -x <level>`.

Then `scripts/major-bumps-level.sh` re-reads each crate's **direct** `libdd-*`
dependency requirements at `prev_tag` vs. the proposal tree and forces `major` where a
requirement's major digit increased — this is how "protobuf 3→4" propagates to its
dependents, and how a no-commit crate can still end up in the release.

### Limitations you must review by hand

`semver-level.sh` looks only at the Rust public API surface. It does **not** know about:

- **Conventional-commit intent.** `feat!:` / `BREAKING CHANGE:` markers are ignored
  entirely. A breaking change that does not alter a signature lands as `patch`.
- **Behavioural breakage.** Same signature, different semantics (defaults, error
  behaviour, panics, wire format, protobuf/proto file changes) ⇒ `patch`.
- **Feature-gated API.** Everything runs with `--all-features`, so API that only exists
  under a non-default feature combination (e.g. `libdd-http-client`'s mutually exclusive
  `reqwest-backend` / `hyper-backend`) is analysed in exactly one configuration.
- **`cargo-semver-checks` false negatives** — notably parameter type changes on
  non-generic functions (`function_parameter_type_changed` is unimplemented). The
  `cargo-public-api` pass exists to cover that; it needs **`cargo-public-api >= 0.52.0`**
  (older versions include parameter names, so a harmless *rename* is promoted to major).

### ⚠️ Review every `minor` and `patch` bump by hand

Every limitation above fails in the same direction: it **under**-estimates the level. So
the bumps that need scrutiny are the low ones.

- **`patch` / `minor` — dangerous.** A missed breaking change published as a patch or
  minor silently breaks consumers on `cargo update`. Read the commits listed for that
  crate in the PR body and ask whether any of them changes behaviour, an FFI layout, a
  wire format, or an API under a feature the analysis did not exercise. If so, raise the
  level on the proposal branch before merging.
- **`major` — safe to accept.** An over-estimated major only costs a version number;
  consumers must opt in, so nothing breaks. Never argue a `major` down to save a digit.
- **Don't use the `!` marker as your verdict.** It is per-PR, while a PR usually touches
  several crates: a `feat!:` in the list says *something* in that PR breaks, not that it
  breaks for every crate the PR modified, and not which one. Treat a `!` under a crate as
  a prompt to read that crate's slice of the diff — and remember the converse, that a
  commit with no `!` can still be breaking for one of the crates it touches.

⇒ Sanity-check each bump in the PR body against its listed commits, spending the effort on
the `patch` and `minor` rows. The `/release-proposal-pr-review-bumps` skill does exactly
this review.

## 3. Review the proposal PR

`release-proposal-test.yml` runs on every push to `release-proposal/**` and on PRs based
on `release/**`:

1. `scripts/crates-to-package.sh` (base = PR base / merge-base with `main`) lists
   publishable crates whose **own** version changed, then `cargo +1.92.0 package` them.
   Cargo ≥ 1.92 is required because sibling versions are not on crates.io yet.
2. Resolves the newest patch of the 3 most recent `datadog-opentelemetry-v*` release
   lines in `DataDog/dd-trace-rs`, and builds each of them with
   `--config patch.crates-io.<crate>.path=...` pointing at the unpacked `.crate` files.
   The log also prints duplicated `libdd-*` versions in the tree — check it.

Note the PR is a **draft**: mark it ready before merging. Its `skip-*` labels disable the
metadata/changelog/PR-title checks that do not apply to release commits.

## 4. Publish (GitLab)

Squash-merge the proposal PR into the ephemeral `release/<crates>/<timestamp>` branch.
That push is mirrored to `gitlab.ddbuild.io/DataDog/libdatadog`, whose `.gitlab-ci.yml`
triggers libddprof-build with `LIBDATADOG_IS_RELEASE_BRANCH=true` (branch matches
`^release/` or `^hotfix/`).

`publish_cargo_crates` is created only when **both** hold
(`.rules_run_on_module_release`):

- `LIBDATADOG_IS_RELEASE_BRANCH == "true"`, and
- `LIBDATADOG_COMMIT_TITLE =~ /chore\(release\): proposal/` — i.e. the squash commit must
  keep the PR title. **Do not** merge with a merge commit.

It is a **manual** job with `DRY_RUN: "true"`. Run it as-is first (it validates versions,
runs the tests and `cargo publish --dry-run`), then re-run it with `DRY_RUN=false` to
publish for real. In order, per crate (`publish-crates.sh`, in publication order):

1. tag version must equal the manifest version;
2. skip if that version is already on crates.io;
3. `cargo nextest --no-default-features` (warn only) and `--all-features` (**blocking**),
   excluding `tracing_integration_tests::`;
4. `cargo publish --all-features`, then add the `github:datadog:libdatadog-owners` owner.

Only **after every crate in the batch succeeds** are the annotated GitHub tags
`<crate>-v<version>` created on the release-branch commit. Finally
`create_pr_to_merge_release_branch.sh` opens a draft PR `release/... → main` (skipped for
hotfixes) — merge it so the bumps and CHANGELOGs land on `main`, and delete the ephemeral
branch (a leftover `release/*/*` blocks the next proposal).

## Hotfixes

> ⚠️ **Untested path.** Every stage below is implemented — the dispatch workflow, the
> GitLab publish rule and the cleanup steps all special-case hotfixes — but the flow has
> never been exercised end to end on a real hotfix.

Pass `main_start_ref = hotfix/<crate>/<N>.x.x` (the branch must exist on origin) and
**exactly one crate**. Differences:

- the hotfix branch *is* the ephemeral branch — nothing new is created and it is never
  deleted by the cleanup steps;
- crates whose tag is not the latest are **not** skipped;
- no merge-back PR to `main` is opened.

Since the proposal PR targets `hotfix/**` and not `release/**`, only the `push`-triggered
half of `release-proposal-test.yml` runs for it.

## Cancelling / retrying a proposal

The dispatch job cleans up both branches on failure. If a run half-succeeded or the
proposal is wrong: close the PR and delete **both** `release-proposal/<...>` and
`release/<...>` on origin, then dispatch again. The ongoing-proposal guard will keep
failing until both are gone.

If publication failed **after** some crates were published, those versions are on
crates.io but no GitHub tags exist. Do not re-publish them — start a new proposal for the
remaining crates from the same commit the release branch was cut from (the failure output
prints that merge-base).

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `Error: A release proposal is ongoing` | A leftover `release-proposal/*` or `release/*/*` branch on origin. Delete it (or wait for the merge-back PR). |
| `Error: unknown or unpublishable crate(s)` | Typo, or the crate has `publish = false`. The error prints the valid list. |
| `Error: resolved commit ... is not reachable from origin/main` | `main_start_ref` points outside `main` / the hotfix branch. Use a commit on a trusted branch. |
| `Error: <crate> is not a 0.1.0 release` | No `<crate>-v*` tag exists, so the run treats it as an initial release. Set the manifest version to `0.1.0` or create the missing tag. |
| `No changes to push. Cancelling the workflow.` | Nothing to release: no commits touched the selected crates' directories since their tags (remember: path-filtered, and `chore(release)` / bot commits are dropped). |
| `Semver level:` followed by a `jq` parse error, or `cargo release version -x` with an empty level | `semver-level.sh` output is parsed as JSON; anything it printed on stderr (it is captured with `2>&1`) breaks the parse. Read the raw step log for the real error, usually a `cargo semver-checks` / `cargo public-api` build failure. |
| `Unexpected exit code from cargo-semver-checks` / `Unexpected error from cargo-public-api` | The crate does not build at the baseline tag or at HEAD with `--all-features`. Reproduce locally: `./scripts/semver-level.sh -v <crate> refs/tags/<crate>-v<version>`. |
| Bump level looks too low in the PR body | Expected for behavioural/ABI/`0.x`-dependency breakage — see the limitations above. A too-low `patch`/`minor` is the failure that matters; edit the version and CHANGELOG on the proposal branch before merging, or close and re-dispatch. A too-high `major` is harmless — leave it. |
| `cargo package` fails in `release-proposal-test.yml` | Usually a sibling crate version not yet on crates.io; the job pins `cargo +1.92.0` for this. Also check for `Cargo.lock` drift and missing files in `include`. |
| dd-trace-rs compile job fails | A real breaking change reaching a consumer — that is the point of the job. Check the "Duplicated dependencies" section for two `libdd-*` majors in one tree. |
| `publish_cargo_crates` is absent from the GitLab pipeline | The merge commit title does not start with `chore(release): proposal` (merge commit instead of squash), or the branch is not `release/**` / `hotfix/**`, or the pipeline was not triggered from the mirror (`CI_PIPELINE_SOURCE != "pipeline"`). |
| `Skipping cargo package: no crates had a version bump` in GitLab | `crates-to-package.sh` compares `LIBDATADOG_COMMIT_BEFORE_SHA..LIBDATADOG_COMMIT_SHA`; a push that carries no version change (e.g. a follow-up commit on the release branch) produces nothing. Re-run the job on the push that contains the bumps. |
| `Version mismatch! tag vs Cargo.toml` | A manual edit desynced the manifest from the computed tag. Fix the manifest on the release branch. |
| `Version X of <crate> is already published` | Skipped, not an error — normal on a re-run. |
| Publication succeeded but no tags | Some crate in the batch failed; tags are created only after the whole batch succeeds. See "Cancelling / retrying" above. |
| Everything published but `main` lacks the bumps | Merge the `release/... → main` draft PR (it is not created for hotfixes — port those manually). |
