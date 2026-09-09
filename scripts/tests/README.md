<!--
Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
SPDX-License-Identifier: Apache-2.0
-->

# Release script tests

Tests for the scripts that drive the release-proposal workflows, plus structural
guards on `.github/workflows/release-proposal-dispatch.yml`.

`release-proposal-dispatch.yml` can only be exercised end to end by running a real
release (or a `bypass_standard_checks` run, which still pushes branches to the real
repository). These tests are the only thing that catches a regression before it ships.

## Running

Requires [bats-core](https://github.com/bats-core/bats-core) >= 1.5.0, plus `jq`,
`git`, `cargo` and `python3` with PyYAML.

```bash
./scripts/tests/run.sh                    # everything (~40s)
./scripts/tests/run.sh semver-level       # one suite
./scripts/tests/run.sh -f "merge-base"    # tests matching a regex
```

## Running against another implementation

Suites never name a script directly — they go through `run_tool NAME`, so the whole
suite can be pointed somewhere else without touching an assertion:

```bash
RELEASE_TOOL_CMD='target/release/release-tool {name}' ./scripts/tests/run.sh
RELEASE_TOOL_CMD='node dist/{name}.js'                ./scripts/tests/run.sh
```

`{name}` is the script's base name (`publication-order`, `release-version-bumps`, …);
the template is split on whitespace, so it can carry a subcommand or flags. This is
what makes the suite usable as a conformance harness if these scripts are ever
rewritten in another language: run both implementations over it and diff.

One suite opts out. `release-version-bumps.bats` doubles `semver-level.sh` by mirroring
`scripts/` into a temp directory and swapping that one file, which relies on the script
resolving siblings relative to how it was invoked. That does not generalise, so those
tests `skip` with a stated reason under `RELEASE_TOOL_CMD` rather than silently running
the real `semver-level.sh`. Giving the script a `--semver-level-cmd` option would close
the gap.

To install bats without root:

```bash
curl -sSfL https://github.com/bats-core/bats-core/archive/refs/tags/v1.11.1.tar.gz \
  | tar xz && ./bats-core-1.11.1/install.sh "$HOME/.local"
```

CI runs the same suite plus `shellcheck` over `scripts/*.sh`
(`.github/workflows/release-scripts.yml`).

## What is covered

| Suite | Covers |
| --- | --- |
| `publication-order.bats` | which crates a release includes, and the order they publish in |
| `commits-since-release.bats` | which commits are attributed to each crate; tag/merge-base resolution; the exported `range`, `latest_tag` and `tag_in_local_branch`; exclusion rules |
| `release-version-bumps.bats` | each crate's fate — released at a level, deferred, skipped, or a hard failure — and the version cargo-release actually lands on |
| `release-version-major-bumps.bats` | which crates get forced to major by a dependency that went major in the same proposal, and which no-commit candidates are pulled back in or dropped |
| `release-generate-changelogs.bats` | the CHANGELOG entry each crate gets — rendered by git-cliff, minimal, or none — and that only the crate's own commits reach it |
| `semver-level.bats` | the bump level fed to `cargo release version -x`, parsed out of cargo-semver-checks and cargo-public-api output |
| `major-bumps-level.bats` | whether a direct `libdd-*` dependency going major forces a dependent to major |
| `release-proposal-workflow.bats` | job-level `if:` gating, untrusted-`main_start_ref` defenses, branch prefixes, push path, PR contract |

## How the suites work

**Fixture workspace** (`helpers/fixture.bash`) — a synthetic six-crate cargo workspace
in a throwaway git repository, so `cargo metadata`, git tags and worktrees are all
real. Its shape is deliberate: a dev-dependency cycle, a `publish = false` crate, a
build-dependency edge and a non-`libdd-*` crate, because those are the cases the
scripts have to discriminate. Nothing touches the libdatadog checkout.

**Cargo shim** (`helpers/cargo-stub/cargo`) — `cargo semver-checks` and
`cargo public-api` cost minutes per call and are precisely the tools whose *output
parsing* `semver-level.sh` gets wrong when it regresses. The shim replays recorded
output for those two subcommands and forwards everything else to the real cargo, so
the script under test runs unmodified.

**Real cargo-release** — the two `release-version-*` suites run `cargo release` against
the fixture workspace rather than stubbing it, so the versions they assert are the ones
a release would land on. They also use it to *set up* state: driving the simulated
previous release through cargo-release rewrites dependents' version requirements the way
a real release does, which a hand-edited manifest does not. Install `cargo-release` to
run them; without it they skip rather than fail.

`release-version-major-bumps.bats` needs no doubles at all — `major-bumps-level.sh` only
reads cargo metadata, with no compilation. `release-version-bumps.bats` doubles just
`semver-level.sh`, by copying the script under test next to a stub sibling: it resolves
`semver-level.sh` relative to itself, so no test-only hook is needed in the script.

**Real git-cliff, real cliff.toml** — `release-generate-changelogs.bats` copies the
repository's own `cliff.toml` into the fixture and lets git-cliff render, so the entries
it asserts are the ones a release would publish rather than an approximation of them.
Install `git-cliff` to run it; without it those tests skip.

**Workflow guards** (`helpers/workflow-to-json.py`) — transcribes the workflow YAML to
JSON so the tests can assert on it with `jq`. These are intentionally
change-detectors: each states *why* the invariant exists, so changing the workflow
means updating the test and reading the reason first. They complement actionlint
(in `lint.yml`), which checks syntax but not intent.

## Adding a test

Assert on behaviour the workflow actually depends on, and say which workflow step
depends on it. Two properties worth preserving:

- Invoke through `run_tool NAME` (or `tool NAME` for a setup step whose output you
  need), never `${SCRIPTS_DIR}/name.sh`. That is what keeps the suite portable.
- Both capture stdout and stderr separately, so `$output` stays parseable JSON when a
  script logs progress. Assert diagnostics with `assert_stderr_mentions`, passing data
  the test supplied — a crate name, a path, an option — rather than a phrase from the
  message. Pinning our exact prose turns every reworded diagnostic into a failure, and
  would force a reimplementation to reproduce the wording instead of the behaviour.
  `assert_stderr_contains` is for text the test itself controls, such as a stub's
  output being checked for forwarding.
- Assert `assert_failure` without a code. The workflow only ever tests zero versus
  non-zero, so a specific status is this implementation's convention, not a contract.
  The exceptions are the tests whose *subject* is that a sub-process's own exit code
  propagates; those name the code deliberately.
- Prefer asserting invariants over golden output. `assert_topological_order` checks
  that every crate follows its dependencies rather than pinning one exact ordering,
  which would break on an irrelevant tie-breaking change.

## Not covered

The four release stages now live in scripts and are covered above. What is still inline
in `release-proposal-dispatch.yml`, and so still out of reach, is the PR body built in
`create-pr` and the branch/ref plumbing in `cargo-release` — checking out the start ref,
creating the ephemeral and proposal branches, and the guards around them.

Making those testable is the same move that produced the four release scripts: lift the
step into a script with its inputs as arguments, A/B it against the previous step body
on a fixture repository, then cover it.

The guards in `release-proposal-workflow.bats` are a holding measure for the
security-critical parts of what is left, not a substitute.
