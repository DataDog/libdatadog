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

Grow the blocking lint surface one check at a time. Do not add a second
formatter. rustfmt owns formatting.

What already runs is [reference.md](reference.md). If a check already
covers the invariant, point at it. Do not duplicate it.

A review comment is lintable when it is mechanical, has a clear yes/no,
and should apply to future PRs. Recast it as an invariant. If the cleanup
is large or opinionated, name the failing files and ask before a
repo-wide rewrite.

## Which tool

Stop at the first fit. One tool per check.

1. **Already enforced** — see reference.md.
2. **Clippy** — a stock rustc/Clippy lint already implements it. Enable
   one lint at `warn` in root `[workspace.lints.clippy]` (CI uses
   `-D warnings`). Prefer tightening an existing allow. Members need
   `[lints] workspace = true`, or match an existing crate-level
   `#![deny]`. The lint must exist on the MSRV channel in
   `rust-toolchain.toml`.
3. **ast-grep** — the invariant is a source-tree shape: attributes,
   calls, casts, items, missing methods, or YAML/TOML/C structure. This
   is the default for a new syntactic check. Add `.sg/rules/<name>.yml`.
4. **Extend an existing linter** — inheritance, bans, crypto graph, or
   generated-file drift. Owners are in reference.md.
5. **CI helper crate** — needs `cargo metadata`, `toml_edit`, or a
   dependency graph. Add it under `.github/actions/` in that workspace,
   not the root workspace. Use `ci-shared`. Path-filtered job in
   `lint.yml`, same pattern as the existing helpers.
6. **Custom script** — a file-set or process invariant that is not an
   AST (artifact presence, CSV drift). `scripts/<name>.sh`, called as
   `bash`, exit 0 to pass. Wire `lint.yml` or a focused workflow.

Do not scan Rust, C, or structured config with regex, `grep`, or a
Python script when an AST match is enough. Regex is fine for non-syntax
data: lockfiles, license rows, `cargo tree` output.

## ast-grep

`./scripts/run-ast-grep.sh` scans the whole repository (tests, then
scan). Do not touch the runner, the pin, or `lint.yml` for a new rule.
Do not diff-scope a rule or turn it off for that scan.

- Write an AST pattern, not a regex. Add `.sg/rules/<name>.yml` and
  `.sg/tests/<name>-test.yml` with valid and invalid fixtures, including
  a comment that mentions the construct but is not code. Refresh with
  `./scripts/run-ast-grep.sh test --update-all`.
- Scope with `files:` / `ignores:`. Skip `target/`, `tests/`,
  `symbolizer-ffi/`, and generated output unless the invariant applies
  there.
- Default severity is `error`. Keep the rule only if the scan exits 0.
  Existing rules and their severities are in reference.md. Do not raise
  a warning rule to `error` unless the whole-repo scan passes.
- Exceptions are an `ignores:` entry or an allow comment with a required
  justification. ast-grep does not expand macros: if the invariant
  applies to generated items, match the `macro_rules` template so every
  expansion is covered.
- The script downloads the pinned binary on first use. Bump
  `AST_GREP_VERSION` and the checksums in `scripts/run-ast-grep.sh`
  together.

## Constraints

- First-party crates only. Exclude `target/`, `vendor/`,
  `symbolizer-ffi/`, `datadog-ipc/{plugins,tarpc}/`, and generated
  output. If a generated file fails, fix the generator.
- Never enable the Clippy `restriction` group wholesale.
- Do not set `allow_failure` or add the job to a flaky list.
- Mechanical cleanup for the new lint's hits is OK in the same change.
  Large or taste-driven cleanup: leave the lint off, name the files, ask.
- New `.rs`, `.sh`, and `Cargo.toml` files need Apache 2.0 headers
  (`./scripts/reformat_copyright.sh`). JSON does not.
- Validate with the commands in AGENTS.md for every crate you touch.
  The lint workflow is `.github/workflows/lint.yml`.

## Other tools

**Clippy.** Run `cargo +stable clippy -p <crate> --all-targets -- -D warnings`
for every touched crate. `--all-features` only when the crate allows it.
Fix the generator if generated files fail. Add the lint to
`allow-annotation-rules` in `clippy-annotation-reporter.yml` only when
the team wants `#[allow]` count drift reported.

**CI helper.** Tests live with the helper. `lint.yml`'s `ci-helpers` job
already formats, clippy-checks, and tests touched helpers. Print
`file:line: message` plus a hint. Support `--annotate` when
`GITHUB_ACTIONS` is set.

**Script.** CWD is the repo root. Print failing paths and why. Do not
scan `target/` or `vendor/`.
