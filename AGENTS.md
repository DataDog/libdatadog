# Libdatadog - shared repository for Datadog rust

**libdatadog** is a Rust workspace of shared libraries and utilities for Datadog's instrumentation tooling (continuous profilers, crash tracking, APM tracing). It exposes C/C++ FFI bindings consumed by Datadog SDKs in other languages.

## Development Workflow

### Toolchains

- **Rust**: MSRV `1.84.1` (set in workspace `Cargo.toml`); use stable for build/clippy.
- **Nightly rustfmt**: `nightly-2026-02-08` — `rustfmt.toml` uses nightly-only features.
- **cargo-nextest**: `0.9.96` (required for running tests). Install with `cargo install --locked 'cargo-nextest@0.9.96'`.
- **cbindgen**: `0.29` (for FFI header generation).
- **System tools**: `cmake` and `protoc`.

### Validating after changes

Iterate fastest with `cargo check -p <crate>` while editing; the full validation steps below are what should be green before declaring work done.

1. **Compile** the touched crates or the workspace but only when doing repo-wide changes:
   ```bash
   cargo check -p <crate>                       # fast iteration on a single crate
   cargo build --workspace --exclude builder    # full build
   ```
2. **Format and lint** — always run on every crate that was touched, before finishing:
   ```bash
   cargo +nightly-2026-02-08 fmt --all -- --check
   cargo +stable clippy --workspace --all-targets --all-features -- -D warnings -A clippy::manual_is_multiple_of
   ```
3. **Run tests** with nextest plus doc tests:
   ```bash
   cargo nextest run --workspace
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

- **crashtracker**: needs `--features libdd-crashtracker/generate-unit-test-files` for unit tests.
- **http-client**: has two mutually-exclusive backend features (`reqwest-backend` is default, `hyper-backend` is the alternative). Both must be exercised when this crate is touched:
  ```bash
  # Default (reqwest) backend — covered by the workspace test run
  cargo nextest run -p libdd-http-client
  # Hyper backend
  cargo nextest run -p libdd-http-client --no-default-features --features hyper-backend,https
  ```
- **test_spawn_from_lib**: `cargo nextest run --package test_spawn_from_lib --features prefer-dynamic`.

## Architecture

The workspace has ~50 crates organized into functional domains:

### Core Infrastructure
- **libdd-common** / **libdd-common-ffi** — HTTP/HTTPS connectors (rustls + ring or aws-lc-rs), container detection, tag validation, rate limiting, Unix/Windows platform helpers
- **libdd-alloc** — custom memory allocators for specialized allocation patterns (profiling, signal-safe contexts)
- **libdd-tinybytes** — `bytes::Bytes`-like type supporting zero-copy cloning and slicing
- **libdd-log** / **libdd-log-ffi** — bridge from Rust's `tracing` infrastructure for use by other languages
- **libdd-telemetry** / **libdd-telemetry-ffi** — telemetry client implementing Datadog's telemetry collection specification
- **libdd-shared-runtime** / **libdd-shared-runtime-ffi** — shared Tokio runtime with fork-safe worker management
- **libdd-capabilities** / **libdd-capabilities-impl** — portable capability traits and native implementations for cross-platform libdatadog
- **libdd-http-client** — HTTP client abstraction with `reqwest-backend` (default) and `hyper-backend` features
- **libdd-dogstatsd-client** — DogStatsD metrics client
- **spawn_worker** — subprocess/worker spawning utilities with platform-specific (Unix/Windows) trampoline mechanisms

### Tracing / APM
- **libdd-data-pipeline** / **libdd-data-pipeline-ffi** — trace exporter; sends data to Trace Agent via msgpack
- **libdd-trace-protobuf** — protobuf definitions for traces
- **libdd-trace-utils** — span processing, MessagePack encoding/decoding, payload handling, HTTP transport with retry
- **libdd-trace-stats** — computes stats from Datadog traces
- **libdd-trace-normalization** — port of the Datadog trace-agent's trace normalization logic
- **libdd-trace-obfuscation** — trace obfuscator implementing Datadog's data security filtering rules
- **libdd-tracer-flare** — collects and transmits tracer diagnostic flares triggered via remote configuration
- **libdd-ddsketch** / **libdd-ddsketch-ffi** — DDSketch quantile estimation

### Profiling
- **libdd-profiling** / **libdd-profiling-ffi** — pprof-format continuous profiling; exports to Datadog via reqwest + tokio
- **libdd-profiling-protobuf** — pprof protobuf definitions (prost-based)
- **libdd-otel-thread-ctx** — OTel thread-level context publisher for profiling (OTEP #4947)
- **datadog-profiling-replayer** — tool that replays a pprof file using libdatadog commands

### Crash Tracking
- **libdd-crashtracker** / **libdd-crashtracker-ffi** — in-process crash detection and reporting; uses blazesym for symbolization on Unix; Windows collector via `collector_windows` feature; also exposes C++ bindings via `cxx` crate
- **symbolizer-ffi** — standalone C/FFI bindings for blazesym (not in workspace members)

### IPC & Sidecar
- **datadog-ipc** / **datadog-ipc-macros** — inter-process communication framework with memory-mapped channel support
- **datadog-sidecar** / **datadog-sidecar-ffi** / **datadog-sidecar-macros** — sidecar process supporting trace collection, profiling, crashtracking, remote config, and live debugging

### Configuration & Remote
- **libdd-library-config** / **libdd-library-config-ffi** — instrumentation library configuration
- **datadog-remote-config** — remote configuration management for dynamic instrumentation and feature toggles
- **datadog-live-debugger** / **datadog-live-debugger-ffi** — live debugger for dynamic inspection

### Feature Flags
- **datadog-ffe** / **datadog-ffe-ffi** — Feature Flags Experiment; includes Python/pyo3 bindings, hence `--all-features` requires Python

### Build, Tooling & Tests
- **builder** — generates release artifacts (C libraries, headers, pkg-config). Run with:
  ```bash
  cargo run --bin release -- --out output-folder
  ```
  Feature flags control what's built: `crashtracker`, `profiling`, `telemetry`, `data-pipeline`, `symbolizer`, `library-config`, `log`, `ddsketch`, `ffe`
- **build-common** — shared cbindgen helpers used by FFI crate `build.rs` scripts (not in workspace members)
- **tools** — dev binaries: header dedup, FFI test runner, JUnit attribute injection
- **tools/cc_utils** — lightweight C compiler utilities for build scripts (kept dependency-free, no `libdd-common`)
- **tools/sidecar_mockgen** — sidecar mock code generator
- **bin_tests** — binary integration test harness (crashtracker, profiling, crash tracking)
- **tests/spawn_from_lib** (package `test_spawn_from_lib`) — tests `spawn_worker` trampoline behavior; requires `prefer-dynamic` feature

## Key Conventions

### Reliability & integrability
libdatadog is integrated into many runtimes and languages via FFI, and runs in Datadog customers' environments. Code should be as reliable and integrable as possible:
- Avoid `unwrap`/`panic!` outside of tests; bubble errors up instead.
- Bubble errors up to the library caller with detail — prefer structured error enums (e.g. `thiserror`) over opaque strings.
- Stay free of global effects unless a feature requires them: no spawning threads, no globals, no reading environment variables behind the caller's back.
- Care about performance, especially memory allocations on hot paths.

### Cryptography
- Non-FIPS builds: ring as TLS crypto provider
- FIPS builds: aws-lc-rs via `fips` feature flag
- Windows FIPS requires env var: `AWS_LC_FIPS_SYS_NO_ASM=1`

### Commit Messages
PR titles and commits must follow **Conventional Commits**: `<type>[scope]: <description>`  
Common types: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`  
Breaking changes: append `!` — e.g. `feat!: remove deprecated API`

### Licenses
All source files must have Apache 2.0 license headers (except `symbolizer-ffi`). The third-party license CSV (`LICENSE-3rdparty.csv`) is validated in CI. To regenerate:
```bash
./scripts/update_license_3rdparty.sh
```

### Release profiles
- `dev`: full debug info
- `release`: size-optimized (`opt-level = "s"`), LTO, single codegen unit
- `bench`: `opt-level = 3`

## Dev Containers

Two devcontainer configurations are provided (Ubuntu and Alpine). They pre-install all required dependencies including cmake, protoc, cbindgen, and Go. See `.devcontainer/`.
