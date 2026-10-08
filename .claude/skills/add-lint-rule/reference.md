# Existing lint and static-analysis tooling

Use this inventory before adding a new check. Prefer extending one of
these over inventing a parallel job.

## Source shape (Rust / C)

| Tool | What it owns | Where |
| --- | --- | --- |
| rustfmt | Formatting only. Nightly, `rustfmt.toml`. | `lint.yml` `rustfmt` job; pre-commit `cargo-fmt`; optional `commit-rustfmt-changes` label |
| Clippy | Stock Rust lints. `-D warnings` on msrv/stable/nightly × linux/windows. | `lint.yml` `clippy` job; root `[workspace.lints.clippy]`; `clippy.toml`; pre-commit `cargo-clippy` (pre-push) |
| crate-level `#![deny(clippy::…)]` | Older per-crate panic/unwrap/expect/todo denials, common on `*-ffi`. | crate `src/lib.rs` |
| `[lints] workspace = true` | Opt-in to `[workspace.lints.*]`. Incomplete across members. | e.g. `libdd-trace-obfuscation/Cargo.toml` |
| clippy-annotation-reporter | Reports `#[allow(clippy::…)]` count drift on PRs. **Not a merge gate** (`continue-on-error`). Matches attributes with regex — do not copy that for new syntactic rules. | `.github/actions/clippy-annotation-reporter`; `.github/workflows/clippy-annotation-reporter.yml` |
| ast-grep | Structural / syntactic rules. Blocking. | `sgconfig.yml`, `.sg/rules/`, `scripts/run-ast-grep.sh`; `lint.yml` `ast-grep` job; pre-commit `ast-grep` |
| ffi-extern-c-panic-containment | `pub extern "C"` fns must contain panics (or a justified allow). Diff-scoped so ~400 legacy accessors stay. | `.sg/rules/ffi-extern-c-panic-containment.yml`; `scripts/run-ffi-panic-lint.sh` (called from `run-ast-grep.sh`) |
| ffi-macro-emits-extern-c | `macro_rules` that emit `extern "C"` must contain panics in the template. Invocations of those macros (e.g. `c_setters!`) are generating sites. | `.sg/rules/ffi-macro-emits-extern-c.yml`; same script |

## Manifests, deps, licenses

| Tool | What it owns | Where |
| --- | --- | --- |
| workspace-deps-lint | Members inherit `[workspace.dependencies]`; workspace entries keep features off. Waivers: `# allow(workspace-deps): …` / `# allow(workspace-deps-features): …`. | `.github/actions/workspace-deps-lint`; `lint.yml` `workspace-deps` job |
| cargo-deny | Advisories, bans, sources. Dedicated always-on bans job. | `deny.toml`; `cargo-deny-bans.yml`; also `pr-metadata-docs-and-deps.yml` |
| cargo-machete | Unused dependencies. | `cargo-machete.yml` |
| check_crypto_providers.sh | No crate may pull both `ring` and `aws-lc-rs` in one runtime graph. | `scripts/check_crypto_providers.sh`; `check-crypto-providers.yml` |
| licensecheck | Apache 2.0 headers on `*.rs` / `*.c` / `*.sh`. | `lint.yml` `licensecheck` job |
| reformat_copyright.sh | Auto-fix those headers. | `scripts/reformat_copyright.sh` |
| dd-rust-license-tool | `LICENSE-3rdparty.csv` matches `Cargo.lock`. | `lint.yml` `license-3rdparty`; `scripts/update_license_3rdparty.sh` |

## Generated files and workflows

| Tool | What it owns | Where |
| --- | --- | --- |
| verify-proto-files | `.proto` sync with datadog-agent + committed `pb.rs` / `pb.idx.rs`. | `verify-proto-files.yml` |
| verify-profiling-heap-sampler-bindings | Committed bindgen output matches regen. | `verify-profiling-heap-sampler-bindings.yml` |
| actionlint (+ shellcheck) | GitHub Actions YAML. | `lint.yml` `actionlint` job |
| CODEOWNERS validator | `CODEOWNERS` files / dup patterns / syntax. | `lint.yml` `codeowners-validator` |
| commitlint | Conventional Commits on PR titles. | `.config/commitlint.config.js` |
| pr-title-semver-check | Semver floor from the PR title vs public API. | `pr-title-semver-check.yml`; `scripts/semver-level.sh` |

## Helper workspace

`.github/actions/` is its own Cargo workspace (`ci-shared`,
`crates-reporter`, `clippy-annotation-reporter`, `workspace-deps-lint`).
`lint.yml`'s `ci-helpers` job rustfmt/clippy/tests whatever those crates
the PR touched. New helper linters belong there, not in the root
workspace.

## Out of scope for this skill

Miri, fuzz, coverage, and FFI example runs are correctness / sanitizer
jobs, not lint rules. Do not fold a new syntactic check into them.
