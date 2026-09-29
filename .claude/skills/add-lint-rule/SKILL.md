---
name: add-lint-rule
description: >-
  Add a Clippy lint, ast-grep rule, CI helper linter, or custom static-analysis
  script to libdatadog's blocking lint CI. Use when the user asks to add a lint
  rule, encode a review comment as a check, enable a workspace Clippy lint,
  write an ast-grep rule, grow lint.yml, or turn a recurring review pattern
  into static analysis.
allowed-tools: Bash Read Grep Glob Edit
---

# Add a lint rule

Grow the blocking lint surface **one check at a time**. Do not add a second
formatter. Do not scan Rust, C, or structured config with regex when an AST
match is enough.

Background: [reference.md](reference.md) (what already runs in CI).

## When a review pattern is lintable

A recurring review comment is a candidate when it is mechanical, has a clear
yes/no, and should apply to future PRs. Recast it as an invariant, then use
the decision list. If the cleanup is large or opinionated, add nothing yet:
name the failing files and ask before a repo-wide rewrite.

## Decide which tool

Walk this list. Stop at the first fit. Do not add two tools for one check.

1. **Already enforced** — rustfmt, an enabled Clippy lint, an existing
   ast-grep rule (including `ffi-extern-c-panic-containment`), cargo-deny,
   cargo-machete, workspace-deps-lint, licensecheck, the crypto-provider
   script, proto/bindings verify jobs, actionlint. Point at the existing
   check; do not duplicate it.
2. **Clippy** — a stock rustc/Clippy lint exists (forbidden construct or
   style Clippy already implements). Edit root `Cargo.toml`
   `[workspace.lints.clippy]`.
3. **ast-grep** — the invariant is a *source-tree shape*: attributes, call
   sites, `as` casts, `extern "C"` without `catch_unwind`, a missing impl
   method, YAML/TOML/C structure. Add `.sg/rules/<name>.yml`.
4. **Extend an existing custom linter** — workspace Cargo.toml inheritance
   → `workspace-deps-lint`. Banned crates/features → `deny.toml`. Crypto
   graph → `scripts/check_crypto_providers.sh`. Generated-file drift → the
   verify-proto / verify-bindings job pattern.
5. **CI helper crate** — you need `cargo metadata`, `toml_edit`, or a
   dependency graph. Add `.github/actions/<name>` like `workspace-deps-lint`.
6. **Custom script** — a file-set or process invariant that is not an AST
   (presence of a generated artifact, CSV drift). Add `scripts/<name>.sh`.

### Regex is the rejected default

Do **not** land a `grep`/`rg`/Python/`regex` crate that matches Rust (or C)
syntax. A PR that regex-scans for a suggested rule will be asked to use
ast-grep instead.

Regex is the wrong tool for attributes, path-qualified names, macros,
comments vs code, and multiline constructs. In-repo example of that
anti-pattern: `clippy-annotation-reporter` matches `#[allow(clippy::…)]`
with a regular expression. New syntactic checks go in ast-grep.

Regex (or `jq` over `cargo metadata`) is fine for *non-syntax* data:
lockfile names, license CSV rows, `cargo tree` output.

## Hard constraints

- Scope is first-party crates. Exclude `target/`, `vendor/`,
  `symbolizer-ffi/`, `datadog-ipc/{plugins,tarpc}/`, and generated output
  (`**/generated/**`, `libdd-trace-protobuf/src/pb.rs`,
  `libdd-trace-protobuf/src/pb.idx.rs`). If a generated file fails, fix
  the generator so regeneration still passes.
- Never enable the Clippy `restriction` group wholesale. Never add a
  second formatter; rustfmt (`rustfmt.toml`, nightly from
  `nightly-toolchain.toml`) owns formatting.
- Do not set `allow_failure` or add the job to a flaky list. Lint blocks
  merge-gate.
- Checks must be MSRV-safe. Clippy runs on msrv/stable/nightly in
  `lint.yml`. A new Clippy lint must exist on the channel in
  `rust-toolchain.toml`, or stay off the MSRV matrix.
- Mechanical cleanup for the new lint's hits is OK in the same change.
  Large or taste-driven cleanup: leave the lint off, name the files, ask.
- New `.rs`, `.sh`, and `Cargo.toml` files need Apache 2.0 headers
  (`./scripts/reformat_copyright.sh`). JSON does not.

## Workflow — Clippy lint

1. Read `[workspace.lints.clippy]` in root `Cargo.toml` and `clippy.toml`.
   Prefer tightening an existing `allow` over adding a new lint.
2. Enable **one** lint at `warn` (CI passes `-D warnings`), for example:

   ```toml
   as_conversions = "warn"
   ```

3. Members honor workspace lints only with `[lints] workspace = true`.
   That opt-in is incomplete today. For a workspace-wide rule, add the
   opt-in on crates that should honor it. Several `*-ffi` crates instead
   use crate-level `#![deny(clippy::…)]` — match the local pattern rather
   than mixing both for the same lint.
4. Run:

   ```bash
   cargo +stable clippy -p <crate> --all-targets -- -D warnings
   ```

   Repeat for every touched crate. Use `--all-features` only when the
   crate allows enabling all features together.
5. Auto-fixable: `cargo clippy --fix -- -D warnings`. If generated files
   fail, fix the generator.
6. Keep the lint only if clippy exits 0 on the intended scope.
7. Add the lint name to `allow-annotation-rules` in
   `.github/workflows/clippy-annotation-reporter.yml` only when the team
   wants `#[allow]` count drift reported for it.

## Workflow — ast-grep rule

ast-grep is already a blocking `lint.yml` job (`./scripts/run-ast-grep.sh`).
Add one YAML rule. Do not touch the runner, the pin, or `lint.yml` unless
this rule cannot pass a whole-repo scan (see "Large existing hit set").

1. Sketch the invariant as an AST pattern, not a regex. Worked example
   already in-tree: `.sg/rules/ffi-extern-c-panic-containment.yml`
   (`kind: function_item` + `extern "C"` + `not` containment macros).
2. Add `.sg/rules/<name>.yml` and `.sg/tests/<name>-test.yml` with both
   valid and invalid fixtures. Cover comments-vs-code (a name in a
   comment is not a match). Refresh snapshots with
   `./scripts/run-ast-grep.sh test --update-all`.
3. Scope with `files:` / `ignores:`. Do not scan `target/`, `tests/`,
   `symbolizer-ffi/`, or generated output unless the invariant applies
   there.
4. Default severity is `error`. Run `./scripts/run-ast-grep.sh`. Keep
   the rule only if it exits 0 on the current tree.

The script downloads the pinned binary into `~/.cache` on first use.
Bump `AST_GREP_VERSION` and the checksums in `scripts/run-ast-grep.sh`
together when upgrading. Do not replace rustfmt or clippy.

### Large existing hit set (diff-scoped)

If a correct rule would fail on a large, intentional legacy set, do
**not** rewrite the repo and do **not** fall back to regex. Copy the
FFI panic rule:

- `severity: warning` so `ast-grep test` still covers it.
- Add `--off=<rule-id>` to the whole-repo `scan` in
  `scripts/run-ast-grep.sh` (already done for
  `ffi-extern-c-panic-containment`).
- Enforce **new signatures only** by intersecting ast-grep JSON hits
  with `git diff -U0` added lines (`scripts/run-ffi-panic-lint.sh`).
  Git decides what is new; ast-grep decides what matched.
- Prefer an explicit allow comment with a required justification
  (`// allow(ffi-panic-boundary): …`) over silent exceptions.

Do not add a second helper crate that parses Rust with regex. That is
what [PR #2588](https://github.com/DataDog/libdatadog/pull/2588) did,
and review asked for ast-grep instead.

## Workflow — extend an existing linter

1. Identify the owner in [reference.md](reference.md).
2. Add one rule or one deny entry. Follow that crate/script's tests
   (`workspace-deps-lint` uses temp-dir fixtures in `tests/cli.rs`).
3. Run the same command CI runs:

   ```bash
   (cd .github/actions && cargo run -p workspace-deps-lint)
   cargo deny check
   ./scripts/check_crypto_providers.sh
   ```

## Workflow — CI helper crate

1. Add `.github/actions/<name>` to the helper workspace in
   `.github/actions/Cargo.toml`.
2. Use `ci-shared` for crate detection, git, or GitHub Actions output.
3. Tests live with the helper. `lint.yml`'s `ci-helpers` job already
   formats, clippys, and tests touched helper crates.
4. Add a dedicated, path-filtered job in `lint.yml` that builds
   `--release` and runs the binary (copy `workspace-deps`).
5. Print `file:line: message` plus a hint. Support `--annotate` when
   `GITHUB_ACTIONS` is set.

## Workflow — custom script

1. Add `scripts/<name>.sh`. License header required. CWD is the repo
   root. Exit 0 to pass, non-zero to fail. Print the failing paths and
   why.
2. The workflow calls `bash scripts/<name>.sh` (do not depend on `+x`).
3. Wire a job in `lint.yml`, or a focused workflow if the check is
   expensive or path-specific (`check-crypto-providers.yml`).
4. Do not scan `target/` or `vendor/`.

## Local commands

```bash
# Format / Clippy (touched crate)
cargo +nightly-2026-07-26 fmt --all -- --check
cargo +stable clippy -p <crate> --all-targets -- -D warnings

# Workspace dependency declarations
(cd .github/actions && cargo run -p workspace-deps-lint)

# Deps / licenses
cargo deny check
cargo machete --with-metadata --skip-target-dir

# Crypto graph
./scripts/check_crypto_providers.sh

# ast-grep (downloads a pinned binary on first use)
./scripts/run-ast-grep.sh
```

The blocking lint workflow is `.github/workflows/lint.yml`. Pre-commit
(`.pre-commit-config.yaml`) mirrors rustfmt, ast-grep, clippy, and the
third-party license check.
