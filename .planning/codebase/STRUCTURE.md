# Codebase Structure

**Analysis Date:** 2026-06-15

## Directory Layout

```
libdatadog/
├── libdd-alloc/                    # Memory allocation utilities
├── libdd-capabilities/             # Feature detection (trait-based, WASM-safe)
├── libdd-capabilities-impl/        # Concrete capability implementation
├── libdd-common/                   # Shared utilities (HTTP, TLS, connectors, errors, tags, rate limiting)
├── libdd-common-ffi/               # FFI primitives (Vec, Slice, Handle, Result, Option, CStr)
├── libdd-crashtracker/             # Core crash tracking (signal handlers, crash info collection)
├── libdd-crashtracker-ffi/         # C/C++ FFI for crash tracking
├── libdd-data-pipeline/            # Message routing, buffering, payload assembly
├── libdd-data-pipeline-ffi/        # FFI for data pipeline
├── libdd-ddsketch/                 # DDSketch quantile summaries
├── libdd-ddsketch-ffi/             # FFI for DDSketch
├── libdd-dogstatsd-client/         # DogStatsD client
├── libdd-http-client/              # HTTP client wrapper (timeout, retry, multipart)
├── libdd-agent-client/             # Agent-specific HTTP client
├── libdd-library-config/           # Endpoint and config overrides
├── libdd-library-config-ffi/       # FFI for library config
├── libdd-log/                      # Logging infrastructure
├── libdd-log-ffi/                  # FFI for logging
├── libdd-otel-thread-ctx/          # OpenTelemetry thread context
├── libdd-otel-thread-ctx-ffi/      # FFI for OTel thread context
├── libdd-profiling/                # Core profiling API and exporter
├── libdd-profiling-ffi/            # C/C++ FFI for profiling (main FFI entry point)
├── libdd-profiling-protobuf/       # Protobuf definitions for profiling
├── libdd-remote-config/            # Remote config agent (RCUR2 protocol)
├── libdd-sampling/                 # Sampling decision logic
├── libdd-shared-runtime/           # Fork lifecycle management infrastructure
├── libdd-shared-runtime-ffi/       # FFI for shared runtime (fork handlers)
├── libdd-telemetry/                # Observability telemetry collection
├── libdd-telemetry-ffi/            # FFI for telemetry
├── libdd-tinybytes/                # Efficient byte strings (ByteStr, ByteVec)
├── libdd-trace-normalization/      # Span tag normalization
├── libdd-trace-obfuscation/        # Span obfuscation (PII scrubbing)
├── libdd-trace-protobuf/           # Protobuf definitions for traces
├── libdd-trace-stats/              # Stats extraction from spans
├── libdd-trace-utils/              # Trace encoding/decoding, HTTP transport, retry logic
├── libdd-tracer-flare/             # Flare collection for troubleshooting
├── datadog-ffe/                    # Feature flag engine (pure Rust, no FFI)
├── datadog-ffe-ffi/                # C/C++ FFI for feature flags
├── datadog-ffe-test-suite/         # FFE test suite
├── datadog-ipc/                    # IPC mechanisms (pipes, sockets)
├── datadog-ipc-macros/             # Macros for IPC message definition
├── datadog-live-debugger/          # Live debugger agent (dynamic probes, PII scrubbing)
├── datadog-live-debugger-ffi/      # FFI for live debugger
├── datadog-profiling-replayer/     # Profile replay tool
├── datadog-sidecar/                # Central hub (span routing, aggregation, dynamic config)
├── datadog-sidecar-ffi/            # Minimal FFI for sidecar
├── datadog-sidecar-macros/         # Macros for sidecar work types
├── builder/                        # Release artifact generator
├── build-common/                   # Shared build utilities
├── spawn_worker/                   # Worker process spawning
├── tools/                          # Development utilities (header dedup, FFI test runner, etc.)
├── symbolizer-ffi/                 # Symbol resolution (native binary)
├── bin_tests/                      # Binary/E2E test suite
├── tests/                          # Integration tests (spawn_from_lib, windows_package)
├── benchmark/                      # Benchmarks
├── fuzz/                           # Fuzzing targets
├── examples/                       # C/C++ FFI examples
├── docs/                           # Documentation
├── Cargo.toml                      # Workspace definition
├── Cargo.lock                      # Dependency lock file
├── .github/                        # GitHub CI workflows
├── .gitlab/                        # GitLab CI config
├── .cargo/                         # Cargo configuration
├── .claude/                        # Claude agent instructions
├── .planning/                      # Planning and analysis documents
├── cmake/                          # CMake build helpers
├── windows/                        # Windows-specific files
└── scripts/                        # Build and utility scripts
```

## Directory Purposes

**libdd-\* domains (profiling, crashtracker, telemetry, data-pipeline, etc.):**
- Purpose: Domain-specific functionality; core logic.
- Contains: Rust-native APIs, data structures, state machines, domain algorithms.
- Key files: `src/lib.rs`, `src/api.rs` (or `src/api/`), `src/exporter.rs`, `src/error.rs`

**libdd-\*-ffi crates:**
- Purpose: C/C++ FFI bindings and opaque handle wrappers.
- Contains: `#[repr(C)]` types, C function signatures, handle wrappers, error conversions.
- Key files: `src/lib.rs` (public FFI API), `src/*_handle.rs` (handle wrappers), `src/error.rs` (error conversion)

**libdd-common:**
- Purpose: Shared infrastructure across all crates.
- Contains: HTTP/HTTPS connectors (reqwest/hyper backends), TLS setup (ring/FIPS), container detection, tag validation, rate limiting, platform helpers, error types.
- Key files:
  - `src/connector/mod.rs` (HTTP/HTTPS setup, TLS provider selection)
  - `src/tag.rs` (tag validation, normalization)
  - `src/config.rs` (configuration structures)
  - `src/error.rs` (shared error types)
  - `src/threading.rs` (platform threading helpers)
  - `src/rate_limiter.rs` (rate limiting)

**libdd-common-ffi:**
- Purpose: FFI type primitives and conversions.
- Contains: Handle, Vec, Slice, Result, Option, CStr, timespec wrappers; validation at boundaries.
- Key files:
  - `src/handle.rs` (opaque pointer wrapper)
  - `src/vec.rs`, `src/slice.rs`, `src/slice_mut.rs` (array ownership)
  - `src/result.rs`, `src/option.rs` (error/value representations)
  - `src/cstr.rs` (C string wrapper)
  - `src/endpoint.rs` (endpoint configuration)

**libdd-trace-utils:**
- Purpose: Trace encoding, HTTP transport, payload handling, retry logic.
- Contains: MessagePack encoding/decoding, batch builder, HTTP transport layer, retry strategy.
- Key files:
  - `src/transport/` (HTTP transport, batching, retry)
  - `src/encoding/` (MessagePack, compression)
  - `src/lib.rs` (main API)

**libdd-profiling:**
- Purpose: Core profiling data structures and export APIs.
- Contains: Profile types, interning APIs, exporter interface, sample collection.
- Key files:
  - `src/api/` (public Rust API)
  - `src/exporter/` (export to pprof format)
  - `src/profiles/` (profile data types)
  - `src/internal/` (internal structures)

**libdd-profiling-ffi:**
- Purpose: C/C++ interface to profiling (main FFI entry point for SDKs).
- Contains: C function signatures, profile handle management, exporter wrapper.
- Key files:
  - `src/lib.rs` (FFI re-exports, module aggregation)
  - `src/arc_handle.rs` (Arc<T> FFI wrapper)
  - `src/exporter.rs` (exporter lifecycle in FFI)

**libdd-crashtracker:**
- Purpose: Crash detection, signal handling, crash info collection.
- Contains: Signal handlers, crash info structures, stacktrace unwinding, demangling stubs.
- Key files:
  - `src/crash_info/` (crash data structures: metadata, stacktraces, spans, telemetry)
  - `src/runtime_callback.rs` (signal handler callback setup)
  - `src/common.rs` (shared crash handling logic)

**libdd-crashtracker-ffi:**
- Purpose: C/C++ FFI for crash tracking.
- Contains: FFI initialization API, crash collection APIs, platform-specific implementations (Unix/Windows).
- Key files:
  - `src/collector.rs` (Unix collector API via `ddog_crasht_init()`)
  - `src/collector_windows/api.rs` (Windows collector via `ddog_crasht_init_windows()`)
  - `src/crash_info/` (crash data structures, mirrors libdd-crashtracker/src/crash_info/)

**datadog-sidecar:**
- Purpose: Central hub for span routing, metric aggregation, remote config polling, feature flag evaluation.
- Contains: Async task coordination (Tokio), work queue management, configuration hot-reload, multi-domain routing.
- Key files:
  - `src/lib.rs` (main sidecar initialization)
  - `src/main.rs` (binary entry point)
  - `src/work/` (work item types and routing)
  - `src/stats/` (stateful aggregation)
  - `src/ffl/` (feature flag logic)

**datadog-sidecar-ffi:**
- Purpose: Minimal C/C++ interface to sidecar (mostly IPC bridging).
- Contains: Span submission API, minimal types.
- Key files:
  - `src/lib.rs` (span submission functions)
  - `src/span.rs` (span representation)

**builder:**
- Purpose: Release artifact generation (C libraries, headers, pkg-config).
- Contains: Cargo build coordination, cbindgen integration, library compilation and packaging.
- Key files:
  - `src/bin/release.rs` (main release builder)
  - `build/main.rs` (build.rs script)

**tools:**
- Purpose: Development utilities.
- Contains: FFI test runner, header dedup, JUnit attribute injection, C++ utilities.
- Key files:
  - `tools/cc_utils/src/` (C++ header utilities)
  - `tools/sidecar_mockgen/src/` (mock generator for tests)

**bin_tests:**
- Purpose: Binary and E2E test suite.
- Contains: Crash collection tests, artifact validation, test harness.
- Key files:
  - `src/test_runner.rs` (test execution harness)
  - `src/modes/behavior.rs` (test behavior definitions)
  - `tests/` (test cases)

**tests/spawn_from_lib:**
- Purpose: Test spawning processes from within a shared library.
- Contains: Fork safety validation, library spawn test cases.

**examples/:**
- Purpose: C/C++ FFI usage examples.
- Contains: Sample FFI code for profiling, crash tracking, telemetry, etc.
- Key files:
  - `examples/ffi/exporter.cpp` (FFI profiling example)
  - `examples/ffi/crashinfo.cpp` (FFI crash tracking example)
  - `examples/ffi/telemetry.c` (FFI telemetry example)

## Key File Locations

**Entry Points:**
- Workspace: `Cargo.toml` (workspace members, shared dependencies, lints)
- Profiling FFI: `libdd-profiling-ffi/src/lib.rs` (main FFI module re-exports)
- Crash tracking FFI: `libdd-crashtracker-ffi/src/lib.rs` (crash FFI module)
- Sidecar library: `datadog-sidecar/src/lib.rs` (async hub initialization)
- Sidecar binary: `datadog-sidecar/src/main.rs` (process entry point)
- Builder: `builder/src/bin/release.rs` (release artifact generation)

**Configuration:**
- Workspace members: `Cargo.toml` (line 5-60)
- Workspace dependencies: `Cargo.toml` (line 82-92)
- Workspace lints: `Cargo.toml` (line 124-155)
- Build profiles: `Cargo.toml` (line 94-114)
- Build config: `build-common/src/lib.rs`, `build-common/build.rs`

**Core Logic:**
- Crash data collection: `libdd-crashtracker/src/crash_info/mod.rs`
- Span routing: `datadog-sidecar/src/work/mod.rs`
- HTTP transport: `libdd-trace-utils/src/transport/mod.rs`
- TLS setup: `libdd-common/src/connector/mod.rs`
- FFI primitives: `libdd-common-ffi/src/` (all modules)

**Testing:**
- Crash tests: `libdd-crashtracker/tests/`
- Profiling tests: `libdd-profiling/tests/`
- E2E tests: `bin_tests/tests/`
- Integration tests: `tests/` (spawn_from_lib, windows_package)

## Naming Conventions

**Files:**
- Domain-specific modules: `libdd-{domain}/src/` (e.g., `libdd-profiling/`, `libdd-crashtracker/`)
- FFI crates: `libdd-{domain}-ffi/` or `datadog-{service}-ffi/` (e.g., `libdd-profiling-ffi/`, `datadog-sidecar-ffi/`)
- Internal modules: `src/internal/`, `src/private/` (not exported from `lib.rs`)
- Platform-specific: `src/platform/{unix,windows}` or conditional compilation via `#[cfg(...)]`

**Directories:**
- `src/api/` — Public Rust API entry points
- `src/ffi/` or directly in `src/lib.rs` — FFI functions (if FFI crate)
- `src/types/` or root — Data structures
- `src/error/` or `src/error.rs` — Error types
- `tests/` — Integration tests at crate level
- `benches/` — Benchmarks
- `examples/` — Usage examples (typically in examples/ at repo root for FFI)

## Where to Add New Code

**New Feature (e.g., new span field, new metric type):**
- Primary code: `libdd-trace-utils/src/` (for trace-related), `libdd-profiling/src/` (for profile-related), or domain-specific crate
- Tests: Co-located in `tests/` directory within the same crate
- FFI bindings: Update corresponding `-ffi` crate (`libdd-trace-utils/src/` doesn't have FFI; go to `datadog-sidecar-ffi/` or nearest domain FFI)

**New Component/Module (e.g., new metric aggregator, new feature flag capability):**
- Rust implementation: Create new crate `libdd-{component}/` with `Cargo.toml`, `src/lib.rs`, and modules
- FFI bindings: Create `libdd-{component}-ffi/` if external SDKs need access
- Registration: Add crate to workspace members in root `Cargo.toml` (line 5-60)
- Features: Add feature flags to `builder/Cargo.toml` if it should be selectable in release builds

**Utilities (shared helpers, macros):**
- Domain-agnostic: `libdd-common/src/` (if not domain-specific)
- Domain-specific: Add module to domain crate (e.g., `src/utils.rs` in `libdd-profiling/`)
- Serialization helpers: `libdd-trace-utils/src/` (for trace-related), `libdd-tinybytes/src/` (for efficient byte types)
- Macros: Create crate `datadog-{macro-name}-macros/` (e.g., `datadog-ipc-macros/`)

**Platform-specific code:**
- Unix: `src/unix/` or `#[cfg(unix)]` modules
- Windows: `src/windows/` or `#[cfg(windows)]` modules
- Examples: `libdd-crashtracker/src/collector_windows/`, `libdd-common/src/threading.rs` (Unix/Windows split)

**Tests:**
- Unit tests: Inline in module (`mod tests { #[test] ... }`) or `tests/` directory in crate
- Integration tests: `tests/` directory (automatically discovered by Cargo)
- E2E tests: `bin_tests/` (for full-system validation)
- Fuzzing: `fuzz/` (define target via `cargo +nightly fuzz list`)

**FFI additions:**
- New C function: Add to domain FFI crate (e.g., `libdd-profiling-ffi/src/lib.rs`), export with `pub extern "C"` signature, declare with `#[no_mangle]`
- New C type: Define in same FFI crate, mark with `#[repr(C)]`, add to generated headers via cbindgen integration in `build-common/Cargo.toml`
- Generated headers: `builder/Cargo.toml` feature flag `cbindgen` triggers header generation; headers output to build artifacts

## Special Directories

**builder/:**
- Purpose: Release artifact generation
- Generated: Yes (outputs C libraries, pkg-config files, headers)
- Committed: No (outputs go to `output/` directory specified at runtime)
- Run with: `cargo run --bin release -- --out output-folder`

**.cargo/:**
- Purpose: Cargo configuration
- Generated: No
- Committed: Yes (lockfile Cargo.lock is committed)

**benchmark/:**
- Purpose: Cargo benchmark suites
- Generated: No
- Committed: Yes
- Run with: `cargo bench -p {crate}`

**fuzz/:**
- Purpose: Fuzzing targets
- Generated: No
- Committed: Yes
- Run with: `cargo +nightly fuzz run {target}`

**.planning/codebase/:**
- Purpose: Codebase analysis documents (ARCHITECTURE.md, STRUCTURE.md, etc.)
- Generated: Yes (via `/gsd-map-codebase` agent)
- Committed: Yes

**examples/:**
- Purpose: C/C++ FFI usage examples
- Generated: No
- Committed: Yes
- Run: See individual example READMEs (e.g., `examples/ffi/README.md`)

**tests/spawn_from_lib/:**
- Purpose: Spawn process tests
- Run: `cargo nextest run --package test_spawn_from_lib --features prefer-dynamic`

**bin_tests/:**
- Purpose: Binary/E2E tests (crash collection, validation)
- Generated: Outputs test artifacts (binaries, crash reports)
- Run: `cargo nextest run -p bin_tests` (requires Docker for tracing tests)

---

*Structure analysis: 2026-06-15*
