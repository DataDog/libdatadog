# Libdatadog - shared repository for Datadog rust

**libdatadog** is a Rust workspace of shared libraries and utilities for Datadog's instrumentation tooling (continuous profilers, crash tracking, APM tracing). It exposes C/C++ FFI bindings consumed by Datadog SDKs in other languages.

## Development Workflow

### Validating after changes

Iterate fastest with `cargo check -p <crate>` while editing; the full validation steps below are what should be green before declaring work done.

1. **Compile** — use `cargo check -p <crate>` for fast iteration on a single crate; run the full workspace build only when changes are repo-wide:
   ```bash
   cargo check -p <crate>                       # fast iteration on a single crate
   cargo build --workspace --exclude builder    # full build (repo-wide changes only)
   ```

2. **Format and lint** — always run on every crate that was touched, before finishing:
   ```bash
   cargo +nightly-2026-02-08 fmt --all -- --check
   cargo +stable clippy --workspace --all-targets --all-features -- -D warnings
   ```

3. **Run tests** with nextest plus doc tests:
   ```bash
   cargo nextest run --workspace --no-fail-fast
   cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib
   cargo test --doc
   ```
   Run a single test by substring: `cargo nextest run -p <crate-name> <test-name>`.

4. **If FFI crates were touched**, build and run the C/C++ FFI examples:
   ```bash
   cargo ffi-test
   ```

5. **If `tracing_integration_tests::` tests fail**, they require Docker. Prompt the user to start Docker and retry; to skip them locally use:
   ```bash
   cargo nextest run -E '!test(tracing_integration_tests::)'
   ```

6. **If `Cargo.lock` was touched**, regenerate the third-party license CSV so `cargo deny` and the CI guard stay green:
   ```bash
   ./scripts/update_license_3rdparty.sh
   cargo deny check
   ```

### Per-crate test notes

- **crashtracker**: needs `--features libdd-crashtracker/generate-unit-test-files` for its unit tests. For the signal-safe collector, validate with `cargo check -p libdd-crashtracker --no-default-features --features collector_signal-safe`, `cargo +stable clippy -p libdd-crashtracker --no-default-features --features collector_signal-safe --all-targets -- -D warnings`, `cargo nextest run -p libdd-crashtracker --no-default-features --features collector_signal-safe --no-fail-fast`, `cargo nextest run -p libdd-crashtracker --features "collector_signal-safe,receiver" --no-fail-fast`, and `bash tools/check_signal_safe_symbols.sh`.
- **http-client**: ships two alternative backend features (`reqwest-backend` is the default, `hyper-backend` is the alternative). Cargo does not enforce exclusivity, but each backend must be exercised independently when this crate is touched:
  ```bash
  # Default (reqwest) backend — covered by the workspace test run
  cargo nextest run -p libdd-http-client
  # Hyper backend
  cargo nextest run -p libdd-http-client --no-default-features --features hyper-backend,https
  ```
- **test_spawn_from_lib**: `cargo nextest run --package test_spawn_from_lib --features prefer-dynamic`.

### Code exploration

When searching for code with `grep` or `find`, always exclude the `./target` directory. Only search in it if specifically looking for build artifacts

## Key Conventions

### Reliability & integrability
libdatadog is integrated into many runtimes and languages via FFI, and runs in Datadog customers' environments. Code should be as reliable and integrable as possible:
- Avoid `unwrap`/`panic!` outside of tests; bubble errors up instead.
- Bubble errors up to the library caller with detail — prefer structured error enums (e.g. `thiserror`) over opaque strings.
- Stay free of global effects unless a feature requires them: no spawning threads, no globals, no reading environment variables behind the caller's back.
- Care about performance, especially memory allocations on hot paths.
- Panics across FFI boundaries are undefined behavior. FFI entry points must catch unwinds (e.g. `std::panic::catch_unwind`) and convert them into error returns rather than letting them propagate into the caller's runtime.
- The C FFI does **not** offer C ABI backward-compatibility guarantees: callers (Datadog SDKs) pin to specific libdatadog versions, so `#[repr(C)]` layouts, function signatures, and enum variants may change between releases.

### Cryptography
- Default build: ring as TLS crypto provider
- FIPS (US government cloud) builds: aws-lc-rs via `fips` feature flag
- Windows FIPS requires env var: `AWS_LC_FIPS_SYS_NO_ASM=1`

### Commit Messages
PR titles and commits must follow **Conventional Commits**: `<type>([scope]): <description>`  
Common types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`  
Breaking changes: append `!` — e.g. `feat!: remove deprecated API`

### Licenses
All source files must have Apache 2.0 license headers (except `symbolizer-ffi`). Use `./scripts/reformat_copyright.sh` to add or fix headers automatically. The third-party license CSV (`LICENSE-3rdparty.csv`) is validated in CI. To regenerate:
```bash
./scripts/update_license_3rdparty.sh
```

### Build Tooling
- **builder** — generates release artifacts (C libraries, headers, pkg-config). Run with:
  ```bash
  cargo run --bin release -- --out output-folder
  ```
  Feature flags control what's built: `crashtracker`, `profiling`, `telemetry`, `data-pipeline`, `symbolizer`, `library-config`, `log`, `ddsketch`, `ffe`
- **tools** — dev binaries: header dedup, FFI test runner, JUnit attribute injection
