@AGENTS.md

<!-- GSD:project-start source:PROJECT.md -->

## Project

**Prophylactic Benchmarking — LLM Analysis Pipeline**

A GitLab CI job in libdatadog that uses Claude (via Datadog's AI Gateway) to analyze benchmark results and post AI-augmented performance reports directly onto libdatadog GitHub PRs. It compares the PR branch against libdatadog `main` to surface regressions, improvements, and suspect code changes — giving contributors instant feedback without waiting for the downstream release cycle.

This is the **"Use LLMs to analyze performance data"** piece of the broader prophylactic benchmarking initiative. The other pieces (cross-repo benchmark triggering, dd-trace-py auto-update) are parallel workstreams by other team members.

**Core Value:** Contributors get benchmark impact feedback on their libdatadog PR before merge, not after a full release cycle.

### Constraints

- Must use Datadog AI Gateway (not direct Anthropic API keys)
- Auth via Vault OIDC JWT → `rapid-ai-platform` audience (same as PHP reference)
- CI image: `registry.ddbuild.io/images/dd-octo-sts-ci-base:2025.06-1` or similar
- GitHub PR comments require dd-octo-sts token scoped to `DataDog/libdatadog`
- No root in CI — install Node/Claude Code via nvm if not pre-installed
- Prototype triggers on every push to a PR branch for easy iteration

<!-- GSD:project-end -->

<!-- GSD:stack-start source:codebase/STACK.md -->

## Technology Stack

## Languages

- Rust 1.87.0 - Core implementation language for all workspace crates, FFI bindings, and shared libraries
- C/C++ - FFI consumers and examples (via cbindgen-generated headers)
- Protobuf - Data serialization format (compiled to Rust via prost)

## Runtime

- tokio 1.23+ (async runtime for networking, multithreading support)
- System native threading and IPC (Unix domain sockets, Windows named pipes)
- cargo (Rust package manager)
- Lockfile: `Cargo.lock` (present, committed)

## Frameworks

- tokio 1.23-1.49 - Async runtime for all async operations
- hyper 1.6 - HTTP/1.1 client and server framework
- prost 0.14.1 - Protocol buffers serialization (tracing and profiling data)
- reqwest 0.13 - HTTP client with rustls TLS (default backend)
- serde/serde_json 1.0 - Serialization/deserialization
- futures 0.3 - Async utilities and utilities for composing async code
- tokio-util 0.7 - Tokio utilities (codec, framing)
- manual_future 0.1.1 - Manual future composition
- crossbeam-queue 0.3 - Lock-free queue for IPC
- cbindgen 0.29 - C header generation from Rust code (feature-gated via `cbindgen` feature)
- cmake 0.1.50 - Build system for C/C++ examples and cross-compilation
- prost-build 0.14.1 - Protobuf code generation
- protoc-bin-vendored 3.0.0 - Vendored protoc compiler
- build-common (internal crate) - Shared build helpers
- rustls 0.23 - TLS implementation (no provider by default)
- rustls with ring provider - Default HTTPS: ring as crypto backend
- aws-lc-rs - FIPS-compliant crypto provider (via `fips` feature, Unix only)
- tokio-rustls 0.26 - Async TLS support via tokio
- hyper-rustls 0.27.7 - TLS support for hyper
- rustls-native-certs 0.8.1-0.8.2 - Native certificate store access
- rustls-platform-verifier 0.6 - Platform-specific certificate verification
- hickory-dns - DNS resolver (replaces system resolver for fork safety)
- bolero 0.13 - Property-based fuzzing framework (feature-gated)
- httpmock 0.8.0-alpha.1 - HTTP mock server for testing
- tempfile 3.x - Temporary file management for tests
- serial_test 3.2 - Test serialization utilities

## Key Dependencies

- anyhow 1.0 - Error handling with context
- thiserror 1.0-2.0 - Structured error types with `#[derive]` macros
- libc 0.2 - Bindings to system C library
- bytes 1.4 - Efficient byte buffer utilities for networking
- base64 0.22 - Base64 encoding/decoding
- serde_json 1.0 - JSON serialization with raw value support
- serde_with 3.x - Additional serde helpers
- serde_bytes 0.11.9 - Efficient byte serialization
- serde_yaml 0.9.34 - YAML serialization
- uuid 1.3-1.7 - UUID generation (v4)
- chrono 0.4.31+ - DateTime handling with timezone support
- regex/regex-lite 1.5 - Pattern matching (lite variant for binary size reduction)
- hashbrown 0.15 - Hash map/set implementation
- tracing 0.1 - Structured logging/tracing instrumentation
- tracing-subscriber 0.3.22 - Tracing configuration and output
- tracing-log 0.2.0 - Bridge from tracing to legacy log crate
- tracing-appender 0.2.3 - Rotating file appenders for logs
- console-subscriber 0.5 - tokio-console task introspection (feature-gated)
- sys-info 0.9.0 - OS information (Windows/Unix)
- memory-stats 1.2.0 - Memory usage statistics with statm support
- prctl 1.0.0 - Process control (Linux)
- nix 0.29 - Safe POSIX system call bindings (Unix)
- windows/windows-sys 0.51-0.59 - Windows API bindings
- symbolic-demangle 12.8.0 - Stack frame demangling (Rust, C++, MSVC)
- symbolic-common 12.8.0 - Symbolic debugging utilities
- cadence 1.3.0 - DogStatsD client library
- pico-args 0.5.0 - Lightweight CLI argument parsing
- toml 0.8.19 - TOML parsing/serialization
- cmake 0.1.50 - CMake build system integration
- tar 0.4.45 - TAR archive handling
- function_name 0.3.0 - Get current function name at compile time
- paste 1.0 - Macro paste helper for code generation
- allocator-api2 0.2.21 - Allocator traits
- const_format 0.2.34 - Const string formatting
- flate2 1.0 - gzip/deflate compression
- simd-json 0.14-0.15 - SIMD-accelerated JSON parsing (non-x86 arch)
- rmp-serde 1.3.0 - MessagePack serialization (sidecar IPC)
- bincode 1.3.3 - Binary serialization format
- sha2 0.10 - SHA2 hashing
- zwohash 0.1.2 - Hash function for fast hashing

## Configuration

- Configuration via environment variables:
- `Cargo.toml` workspace manifest with feature flags for:
- `rust-toolchain.toml` - Pinned Rust 1.87.0 with rustfmt and clippy
- `.cargo/config.toml` - Cargo aliases (e.g., `ffi-test`)
- `rustfmt.toml` - Code formatting rules
- `clippy.toml` - Linter configuration
- `.config/nextest.toml` - Test runner configuration
- `deny.toml` - Dependency audit configuration (multiple versions warning)

## Platform Requirements

- Rust 1.87.0 (or newer per MSRV)
- cargo with workspace resolver v2
- cbindgen 0.29 (for FFI header generation)
- cmake 3.x (for C/C++ example builds)
- protoc (protobuf compiler) - can use vendored version via feature
- System C compiler (gcc/clang on Unix, MSVC on Windows)
- Rust version must be compatible with:
- FIPS feature requires `AWS_LC_FIPS_SYS_NO_ASM=1` on Windows
- Nextest 0.9.96 for test execution
- Deployment as shared library (dylib, staticlib, or cdylib)
- Requires Datadog agent (default: localhost:8126) or direct API key for agentless submission
- Optional Docker for integration tests (`tracing_integration_tests`)

<!-- GSD:stack-end -->

<!-- GSD:conventions-start source:CONVENTIONS.md -->

## Conventions

## Naming Patterns

- Snake case: `libdd_http_client`, `libdd_trace_utils`, `span_utils.rs`
- FFI crate suffix: `-ffi` (e.g., `libdd-common-ffi`, `libdd-http-client` exposes FFI via separate `-ffi` crates)
- Module files match module names: `client.rs`, `error.rs`, `config.rs`, `retry.rs`, `request.rs`, `response.rs`
- Snake case: `ensure_crypto_provider()`, `send_traces()`, `send_once()`, `handle_panic_error()`
- Private helper functions prefixed with underscore when needed (e.g., module-private: `fn from_config_and_transport()`)
- Builder methods use chainable names: `base_url()`, `timeout()`, `with_filename()`, `build()`
- Async functions clearly marked: `async fn send()`, `async fn send_with_retry()`
- Getter methods omit `get_` prefix: `config()`, `timeout()`, `retry()` (not `get_config()`)
- Snake case: `base_url`, `retry_config`, `mock_server`, `last_err`, `crypto_provider`
- Field names in structs: snake case (e.g., `treat_http_errors_as_errors: bool`)
- Loop variables conventional: `attempt`, `err`, `delay`
- PascalCase for structs and enums: `HttpClient`, `HttpRequest`, `HttpClientError`, `HttpMethod`, `MultipartPart`
- Error variants as concrete enum members: `HttpClientError::TimedOut`, `HttpClientError::ConnectionFailed(String)`
- Config types: `HttpClientConfig`, `RetryConfig`, `HttpClientBuilder`
- All caps with underscores: `wrap_with_ffi_result!`, `wrap_with_void_ffi_result!`, `wrap_with_ffi_result_no_catch!`
- Decorated with `#[named]` attribute to capture function name for error reporting

## Code Style

- Tool: `rustfmt` (nightly-2026-02-08)
- Config: `rustfmt.toml` at repo root
- Tool: `clippy` (stable)
- Config: `clippy.toml` at repo root

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

- **Production code must not:**
- **Exception:** `unwrap_or_else()` is acceptable for fallback error handling (e.g., `last_err.unwrap_or_else(|| HttpClientError::...)`), not flagged as `unwrap_used`
- **FFI entry points:** Must wrap with `catch_unwind` and `wrap_with_ffi_result!` macro
- All public items require doc comments via `#![deny(missing_docs)]`
- Doc comments explain the public API, not implementation details
- Examples show usage in doc comments when helpful
- Library modules document module-level purpose with module-level doc comments

## Import Organization

- Barrel exports at crate root (`lib.rs`) expose public types:
- Private modules marked with `mod` (e.g., `mod client; mod error;`)
- Public modules marked with `pub mod` for re-export (e.g., `pub mod config; pub mod retry;`)

## Error Handling

- Define enum with `#[derive(Debug, Error)]` from `thiserror`
- Each variant has error display message via `#[error(...)]` attribute
- Variants may contain structured data (e.g., status code, body text)

#[derive(Debug, Error)]

- Use `Result<T, ErrorType>` (not `Option<T>`)
- Return results all the way up; catch/handle at boundaries only
- Bubble errors with context using `anyhow::Context` trait (`context()` method)
- FFI crates define `Error` struct that wraps `Vec<u8>` (FFI-safe string buffer)
- Convert `anyhow::Error` to FFI `Error` via `From<anyhow::Error>` impl
- Handle panics in FFI entry points with `catch_unwind` and convert to error returns
- Never let panics propagate across FFI boundaries (undefined behavior)

## Logging

- Avoid logging in hot paths (performance-critical sections)
- Library code typically does not log; let the caller control logging
- If logging is needed, use structured logging where possible
- No println! in production library code (stderr/stdout pollution)

## Comments

- Explain *why*, not *what* (code shows what)
- Document non-obvious behavior, safety invariants, FFI considerations
- Mark platform-specific code: `#[cfg(unix)]`, `#[cfg(windows)]`
- Explain algorithm complexity or performance rationale
- Document panics/abort conditions in tests only
- Required for all public items via `#![deny(missing_docs)]`
- Format: `/// Single-line summary` or multi-line with `///`
- Code examples in docs wrapped with ` ```rust ` and ` ``` `
- Use `#[example]` for longer runnable examples
- Safety invariants documented with `// Safety:` comments in unsafe blocks

## Function Design

- Keep functions focused on a single responsibility
- Typical range: 20-50 lines for public functions; smaller for helpers
- Long async functions acceptable if clear control flow (e.g., retry loops)
- Use builder pattern for many parameters (e.g., `HttpClientBuilder`)
- Prefer `impl Into<T>` for string-like conversions: `name: impl Into<String>`
- Async functions return `async fn() -> Result<T, E>`
- Always use `Result<T, E>` (never `Option<Result<...>>`)
- Return early with `?` operator
- Chain methods on builders (consume self, return self)

## Module Design

- Crate root (`lib.rs`) re-exports public API via `pub use`
- Module boundaries hide implementation (e.g., `backend/` is `pub(crate)`)
- Private modules grouped by feature or domain
- Crate root `lib.rs` acts as barrel file
- Does *not* re-export internal modules; only the public API types
- `pub mod config;` — re-exports module at crate root
- `mod backend;` — private implementation detail
- `pub(crate) fn from_config()` — internal to crate, not in public API

## Async/Await

- Use `tokio::test` for async unit tests: `#[tokio::test] async fn test_foo() { ... }`
- Use `tokio::spawn` when spawning tasks (rare in this codebase; prefer single-threaded)
- Never spawn threads in library code unless feature-gated; let the caller control concurrency
- Use `async fn` for all I/O-bound operations

## Concurrency & Globals

- No static mutable variables in production code
- Exception: `catch_unwind` in FFI entry points (macro handles safely)
- Exception: Feature-gated cryptographic provider initialization (caller responsible)
- Thread-safe via immutable references; no locks in hot paths
- Called once at startup: `libdd_http_client::init_fips_crypto()?`
- Returns error if provider already installed (safety check)
- Caller ensures single initialization

## Testing Patterns

- Tests can use `unwrap()`, `expect()`, `panic!()` (allowed by clippy.toml)
- Unit tests in `#[cfg(test)]` modules within source files
- Integration tests in `tests/` directory at crate root
- Async tests use `#[tokio::test]` attribute
- Doc tests run via `cargo test --doc`

<!-- GSD:conventions-end -->

<!-- GSD:architecture-start source:ARCHITECTURE.md -->

## Architecture

## System Overview

```text

```

## Component Responsibilities

| Component | Responsibility | File |
|-----------|----------------|------|
| libdd-profiling | Core CPU/heap/etc profiling APIs and data types; exporter interface | `libdd-profiling/src/api/`, `libdd-profiling/src/exporter/` |
| libdd-profiling-ffi | C/C++ FFI bindings and handle wrappers for profiling; aggregates all other FFI modules as optional re-exports | `libdd-profiling-ffi/src/lib.rs` |
| libdd-crashtracker | Rust-side crash detection, signal handling, crash info collection (stack traces, metadata) | `libdd-crashtracker/src/crash_info/`, `libdd-crashtracker/src/runtime_callback.rs` |
| libdd-crashtracker-ffi | C/C++ FFI API for crash tracking; Unix and Windows implementations; demangling | `libdd-crashtracker-ffi/src/collector.rs`, `libdd-crashtracker-ffi/src/crash_info/` |
| libdd-telemetry | Observability telemetry collection and submission | `libdd-telemetry/src/` |
| libdd-telemetry-ffi | C/C++ FFI for telemetry | `libdd-telemetry-ffi/src/` |
| libdd-data-pipeline | Message routing, filtering, payload assembly for multi-domain aggregation in the sidecar | `libdd-data-pipeline/src/` |
| libdd-data-pipeline-ffi | C/C++ FFI for data pipeline (spans, metrics, traces) | `libdd-data-pipeline-ffi/src/` |
| datadog-sidecar | Central hub for span routing, metric aggregation, dynamic config, feature flags; coordinates work from all domains | `datadog-sidecar/src/` |
| datadog-sidecar-ffi | Minimal C/C++ interface to sidecar (mostly IPC for span submission) | `datadog-sidecar-ffi/src/` |
| datadog-live-debugger | Live debugger agent (dynamic probes, local PII scrubbing) | `datadog-live-debugger/src/` |
| libdd-trace-utils | Trace encoding/decoding (MessagePack), HTTP transport, payload building, retry logic | `libdd-trace-utils/src/` |
| libdd-trace-normalization | Span tag normalization (removes invalid tags, applies conventions) | `libdd-trace-normalization/src/` |
| libdd-trace-obfuscation | Span obfuscation (PII scrubbing, secret redaction) | `libdd-trace-obfuscation/src/` |
| libdd-trace-protobuf | Protobuf message definitions for spans, metrics, and trace data | `libdd-trace-protobuf/src/` |
| libdd-trace-stats | Stats extraction from spans (service, env, resource) | `libdd-trace-stats/src/` |
| libdd-common | Shared utilities: HTTP/HTTPS connectors (reqwest/hyper), TLS (ring/FIPS), container detection, tag validation, rate limiting, platform helpers | `libdd-common/src/connector/`, `libdd-common/src/tag.rs` |
| libdd-common-ffi | FFI primitives: type wrappers (Vec, Slice, Handle, Result, Option, CStr, timespec) | `libdd-common-ffi/src/` |
| libdd-http-client | Thin HTTP client wrapper (timeout, retry, multipart support) | `libdd-http-client/src/` |
| libdd-agent-client | HTTP client for talking to the Datadog agent | `libdd-agent-client/src/` |
| libdd-capabilities | Feature detection API (thread-safe, WASM-safe) | `libdd-capabilities/src/` |
| libdd-capabilities-impl | Concrete capability implementation (not WASM) | `libdd-capabilities-impl/src/` |
| libdd-tinybytes | Efficient byte strings (ByteStr, ByteVec) for serialization | `libdd-tinybytes/src/` |
| libdd-ddsketch | DDSketch quantile summaries for metrics | `libdd-ddsketch/src/` |
| libdd-ddsketch-ffi | FFI for DDSketch | `libdd-ddsketch-ffi/src/` |
| libdd-sampling | Sampling decision logic | `libdd-sampling/src/` |
| libdd-tracer-flare | Flare collection for troubleshooting | `libdd-tracer-flare/src/` |
| libdd-remote-config | Remote config agent (RCUR2 protocol) | `libdd-remote-config/src/` |
| datadog-ffe | Feature flag engine (pure Rust, no FFI) | `datadog-ffe/src/` |
| datadog-ffe-ffi | C/C++ FFI for feature flags | `datadog-ffe-ffi/src/` |
| libdd-library-config | Endpoint and configuration overrides | `libdd-library-config/src/` |
| libdd-library-config-ffi | FFI for library config | `libdd-library-config-ffi/src/` |
| libdd-log-ffi | FFI for logging | `libdd-log-ffi/src/` |
| libdd-otel-thread-ctx-ffi | OpenTelemetry thread-local context storage (trace/span ID) | `libdd-otel-thread-ctx-ffi/src/` |
| libdd-shared-runtime-ffi | Fork lifecycle management (prepare, atfork, postfork) | `libdd-shared-runtime-ffi/src/` |
| symbolizer-ffi | Symbol resolution (native binary) | `symbolizer-ffi/src/` |
| builder | Release artifact generator (builds C libraries, headers, pkg-config via cargo run --bin release) | `builder/src/bin/release.rs` |
| datadog-ipc | IPC mechanisms (pipes, sockets) for sidecar communication | `datadog-ipc/src/` |
| datadog-ipc-macros | Macros for IPC message definition | `datadog-ipc-macros/src/` |
| datadog-sidecar-macros | Macros for sidecar work types | `datadog-sidecar-macros/src/` |
| tools | Development utilities (header dedup, FFI test runner, JUnit attribute injection) | `tools/src/`, `tools/cc_utils/`, `tools/sidecar_mockgen/` |

## Pattern Overview

- **No global state in libraries:** Pure function design except where necessary (connectors, TLS providers). Callers explicitly initialize what they need.
- **FFI safety:** All FFI entry points deny panics, unwrap, and expect. Error returns use `Result` wrappers. Panics across FFI boundaries are caught with `catch_unwind`.
- **Feature-gated domains:** The builder selects which domains to compile (e.g., `crashtracker`, `profiling`, `telemetry`) to minimize binary size.
- **Async-first (Tokio):** Most I/O uses async/await with Tokio runtime, but keeps the Rust APIs synchronous where possible to simplify FFI.
- **Error types:** Structured error enums (via `thiserror`) bubble up through layers; FFI crates convert them to C-compatible status codes/strings.

## Layers

- Purpose: Expose Rust functionality to C/C++ callers via C ABI with struct/enum marshaling, opaque handle pointers, and generated headers.
- Location: `libdd-profiling-ffi/`, `libdd-crashtracker-ffi/`, `libdd-telemetry-ffi/`, `libdd-data-pipeline-ffi/`, `datadog-sidecar-ffi/`, etc.
- Contains: `#[repr(C)]` types, C function signatures, handle wrappers, conversion from Rust types to C-compatible representations.
- Depends on: Corresponding domain crates (libdd-profiling, libdd-crashtracker, etc.) + libdd-common-ffi for FFI primitives.
- Used by: C/C++ SDKs (via generated headers from cbindgen).
- Purpose: Implement concrete logic for profiling, crash tracking, telemetry, data routing, etc.
- Location: `libdd-profiling/`, `libdd-crashtracker/`, `libdd-telemetry/`, `libdd-data-pipeline/`, `datadog-sidecar/`, etc.
- Contains: Rust-native APIs, data collectors, state machines, async coordination, integration with lower-level utilities.
- Depends on: Shared infrastructure (libdd-common, libdd-trace-utils, serialization crates), platform-specific modules for Windows/Unix.
- Used by: Domain FFI crates + other domain crates (e.g., sidecar uses all domains).
- Purpose: Provide HTTP transport, TLS/crypto, serialization, error handling, tag validation, rate limiting, platform abstraction.
- Location: `libdd-common/`, `libdd-http-client/`, `libdd-trace-utils/`, `libdd-common-ffi/`, `libdd-capabilities*`, serialization crates.
- Contains: Connectors (reqwest/hyper backends, HTTPS with ring or FIPS crypto), platform APIs (Unix signals, Windows APIs), test utilities.
- Depends on: External crates (tokio, serde, prost, rustls, hyper, ring/aws-lc-rs).
- Used by: All domain crates.
- Purpose: Define data encodings (MessagePack, Protobuf), efficient byte representations, sampling rules, config structures.
- Location: `libdd-tinybytes/`, `libdd-trace-protobuf/`, `libdd-sampling/`, `libdd-ddsketch/`, `libdd-library-config/`, etc.
- Contains: Serde-derived structs, Protobuf definitions (compiled via prost), sketches, enum variants for config.
- Depends on: serde, prost, rmp-serde, base64, etc.
- Used by: All layers above.

## Data Flow

### Primary Request Path: Span Submission (Traces)

### Crash Collection Path

### Profile Submission Path

- **Buffering:** Spans and profiles buffered in memory via `libdd-data-pipeline/src/buffering/` pending HTTP submission.
- **Deduplication:** Sidecar applies dedup logic to reduce redundant spans.
- **Sidecar coordination:** `datadog-sidecar/src/` maintains async task queues (Tokio channels) for each domain; work items are pulled by submission tasks.

## Key Abstractions

- Purpose: Opaque pointer type for FFI, prevents accidental access to Rust objects from C code.
- Examples: `libdd-common-ffi/src/handle.rs`, `libdd-profiling-ffi/src/arc_handle.rs`
- Pattern: `struct DdProf<T>(*mut T)` with `#[repr(transparent)]` to ensure FFI compatibility.
- Purpose: Convert Rust `Result<T>` to C-compatible `DdProfError` or status codes.
- Examples: `libdd-common-ffi/src/result.rs`, `libdd-profiling-ffi/src/profile_error.rs`
- Pattern: FFI functions return `DdProfError`, callers check `.is_ok()` or inspect error details.
- Purpose: Safe FFI ownership of arrays and dynamic vecs.
- Examples: `libdd-common-ffi/src/slice.rs`, `libdd-common-ffi/src/vec.rs`
- Pattern: `Slice<T>` for borrowed arrays (ptr + len), `Vec<T>` for owned dynamic vecs with FFI-safe lifetime management.
- Purpose: Safe C string ownership and UTF-8 validation.
- Examples: `libdd-common-ffi/src/cstr.rs`
- Pattern: `CStr` validated at boundaries, auto-dropped when returned from Rust.
- Purpose: Efficient serialization of sidecar work items.
- Examples: `datadog-ipc/src/`, `datadog-ipc-macros/src/`
- Pattern: Define message structs with `#[ipc(..)]` macro, serialized via bincode or MessagePack.
- Purpose: Feature detection and conditional logic without runtime overhead.
- Examples: `libdd-capabilities/src/`
- Pattern: Thread-safe enum of capability states; allows graceful degradation when features unavailable.

## Entry Points

- Location: `libdd-profiling-ffi/src/lib.rs` (FFI functions) + `libdd-profiling-ffi/src/arc_handle.rs` (handle wrappers)
- Triggers: Language SDK calls C functions (e.g., `ddog_prof_...`)
- Responsibilities: Accept profiles from native code, manage lifecycle, expose interning APIs, export profiles, manage exporters.
- Location: `libdd-crashtracker-ffi/src/collector.rs`
- Triggers: Installed as signal handler via `ddog_crasht_init()`
- Responsibilities: Intercept SIGSEGV/SIGABRT/SIGBUS/etc., collect crash data, serialize and submit.
- Location: `libdd-crashtracker-ffi/src/collector_windows/api.rs` (`ddog_crasht_init_windows`)
- Triggers: Installed by SDK at runtime
- Responsibilities: Hook Windows exception handler, collect unhandled exception data.
- Location: `datadog-sidecar/src/main.rs` (or as library via `datadog-sidecar/src/lib.rs`)
- Triggers: Spawned by language SDK as separate process or linked as library
- Responsibilities: Central hub for span routing, metric aggregation, remote config polling, feature flag evaluation, dynamic configuration.
- Location: `datadog-sidecar-ffi/src/lib.rs`
- Triggers: Language SDK calls via IPC
- Responsibilities: Span submission (minimal interface, mostly IPC bridging).
- Location: `libdd-telemetry-ffi/src/lib.rs`
- Triggers: Language SDK calls telemetry functions
- Responsibilities: Collect and submit observability telemetry.
- Location: `libdd-library-config-ffi/src/lib.rs`
- Triggers: SDKs request config overrides
- Responsibilities: Parse and expose endpoint overrides, proxy settings, etc.

## Architectural Constraints

- **Threading:** Tokio runtime (multi-threaded by default) used in sidecar and domain crates for I/O coordination; FFI calls must not block the runtime.
- **Global state:** Avoided in library crates. Sidecar maintains global async runtime; domain crates accept context/config at initialization.
- **Circular imports:** Rare; potential cycles include sidecar → data-pipeline → trace-utils → common (resolved via feature gates).
- **FFI panic safety:** All public FFI functions must deny panic/unwrap/expect outside tests; FFI entry points wrap Rust logic in `catch_unwind`.
- **ABI stability:** No C ABI backward-compatibility guarantees; callers pin to libdatadog versions. `#[repr(C)]` struct layouts may change between releases.
- **Memory ownership:** FFI types use explicit ownership (borrowed via `Slice<T>`, owned via `DdProf<T>` or `ddog_malloc`). No automatic deallocation across FFI.
- **FIPS compliance:** Optional FIPS mode (aws-lc-rs crypto) for US government cloud; feature flag selects TLS provider (ring vs. aws-lc-rs).

## Anti-Patterns

### Blocking in Async Context

### Unwrap/Panic Outside Tests

### Global Mutable State

### Ignoring Fork Safety

### Assuming Synchronous Behavior

## Error Handling

- **Domain crates:** Use `Result<T, DomainError>` where `DomainError` is an enum variant or `anyhow::Error`.
- **FFI crates:** Convert to `DdProfError` or status code; return error details via out-parameters or error string accessors.
- **Panic safety:** FFI entry points wrap Rust logic in `std::panic::catch_unwind()`, convert panics to `DdProfError::Internal`.

## Cross-Cutting Concerns

<!-- GSD:architecture-end -->

<!-- GSD:skills-start source:skills/ -->

## Project Skills

| Skill | Description | Path |
|-------|-------------|------|
| create-release | Bump the Rust workspace version in root Cargo.toml, regenerate the lockfile, and open a draft PR on GitHub. Use this skill whenever the user says something like "create a release", "bump the version", "release vX.Y.Z", "prepare a release branch", or "bump workspace version". Trigger even if they just say "release X.Y.Z" or mention a semver version in a release context. | `.claude/skills/create-release/SKILL.md` |
<!-- GSD:skills-end -->

<!-- GSD:workflow-start source:GSD defaults -->

## GSD Workflow Enforcement

Before using Edit, Write, or other file-changing tools, start work through a GSD command so planning artifacts and execution context stay in sync.

Use these entry points:

- `/gsd-quick` for small fixes, doc updates, and ad-hoc tasks
- `/gsd-debug` for investigation and bug fixing
- `/gsd-execute-phase` for planned phase work

Do not make direct repo edits outside a GSD workflow unless the user explicitly asks to bypass it.
<!-- GSD:workflow-end -->

<!-- GSD:profile-start -->

## Developer Profile

> Profile not yet configured. Run `/gsd-profile-user` to generate your developer profile.
> This section is managed by `generate-claude-profile` -- do not edit manually.
<!-- GSD:profile-end -->
