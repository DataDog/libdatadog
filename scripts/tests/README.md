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
./scripts/tests/run.sh                    # everything (~15s)
./scripts/tests/run.sh semver-level       # one suite
./scripts/tests/run.sh -f "merge-base"    # tests matching a regex
```

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
| `commits-since-release.bats` | which commits are attributed to each crate; tag/merge-base resolution; the exported `range`; exclusion rules |
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

**Workflow guards** (`helpers/workflow-to-json.py`) — transcribes the workflow YAML to
JSON so the tests can assert on it with `jq`. These are intentionally
change-detectors: each states *why* the invariant exists, so changing the workflow
means updating the test and reading the reason first. They complement actionlint
(in `lint.yml`), which checks syntax but not intent.

## Adding a test

Assert on behaviour the workflow actually depends on, and say which workflow step
depends on it. Two properties worth preserving:

- Use `run --separate-stderr` so `$output` stays parseable JSON when a script logs
  progress to stderr; assert diagnostics with `assert_stderr_contains`.
- Prefer asserting invariants over golden output. `assert_topological_order` checks
  that every crate follows its dependencies rather than pinning one exact ordering,
  which would break on an irrelevant tie-breaking change.

## Not covered

The inline bash in `release-proposal-dispatch.yml` — the bump-range computation, the
"tag is not the latest" skip rule, the major-bump merge, CHANGELOG generation and the
PR body — is untestable while it lives inside `run:` blocks. Making it testable means
extracting the decision half of each step into a script that takes JSON and git state
and returns JSON. The guards in `release-proposal-workflow.bats` are a holding
measure for the security-critical parts of that logic, not a substitute.
