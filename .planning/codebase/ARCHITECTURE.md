<!-- refreshed: 2026-06-15 -->
# Architecture

**Analysis Date:** 2026-06-15

## System Overview

libdatadog is a Rust workspace of shared libraries and utilities for Datadog's instrumentation tooling. It exposes C/C++ FFI bindings consumed by Datadog SDKs in other languages (Python, Java, Ruby, Node.js, Go, etc.). The architecture follows a layered, modular design where domain crates implement functionality and corresponding FFI crates wrap them for C/C++ interoperability.

```text
┌────────────────────────────────────────────────────────────────────────────┐
│                           C/C++ FFI Layer                                  │
│  (Generated headers via cbindgen + runtime bindings)                        │
├──────────────────┬──────────────────┬─────────────────┬────────────────────┤
│ libdd-profiling  │ libdd-crash      │ libdd-telemetry │ libdd-data-pipeline│
│ -ffi             │ tracker-ffi      │ -ffi            │ -ffi              │
│ `libdd-profiling │ `libdd-crash     │ `libdd-telemetry│ `libdd-data-pipeline
│ -ffi/src/lib.rs` │ tracker-ffi/...` │ -ffi/src/lib.rs`│ -ffi/src/lib.rs`  │
└──────────────────┴──────────────────┴─────────────────┴────────────────────┘
         │                    │                 │                 │
         ▼                    ▼                 ▼                 ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                      Domain Implementation Layer                            │
│   (Rust logic for profiling, crash tracking, tracing, observability)        │
├──────────────────┬──────────────────┬─────────────────┬────────────────────┤
│ libdd-profiling  │ libdd-crashtracker│ libdd-telemetry│ libdd-data-pipeline│
│ `libdd-profiling │ `libdd-crash     │ `libdd-telemetry│ `libdd-data-pipeline
│ /src/api/`       │ tracker/src/...` │ /src/`          │ /src/`             │
│ libdd-trace-utils│ datadog-live-    │                 │ datadog-sidecar    │
│ `libdd-trace-utils│ debugger         │                 │ `datadog-sidecar/  │
│ /src/`           │ `datadog-live-   │                 │  src/`             │
│                  │ debugger/src/`   │                 │                    │
└──────────────────┴──────────────────┴─────────────────┴────────────────────┘
         │                    │                 │                 │
         └────────────────────┴─────────────────┴─────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                       Shared Infrastructure Layer                           │
│    (HTTP, crypto, serialization, error handling, platform abstraction)      │
├─────────────────┬──────────────────┬──────────────┬────────────────────────┤
│ libdd-common    │ libdd-common-ffi │ libdd-trace- │ libdd-capabilities    │
│ `libdd-common/  │ `libdd-common-ffi│ normalization│ `libdd-capabilities/  │
│ src/connector/` │ /src/`           │ `libdd-trace │ src/`                 │
│ `libdd-common/  │ (handles, slices,│ -normalization│ libdd-capabilities   │
│ src/tag.rs`     │ vecs, strings)   │ /src/`       │ -impl (WASM-safe)     │
│ `libdd-common/  │                  │ libdd-trace- │ `libdd-capabilities- │
│ src/config.rs`  │ libdd-http-client│ obfuscation  │ impl/src/`            │
│ `libdd-common/  │ `libdd-http-     │ `libdd-trace │                       │
│ src/error.rs`   │ client/src/`     │ -obfuscation │                       │
│ (HTTP, TLS,     │ (reqwest/hyper   │ /src/`       │                       │
│ DNS, container) │ backends)        │              │                       │
└─────────────────┴──────────────────┴──────────────┴────────────────────────┘
         │                    │                 │                 │
         └────────────────────┴─────────────────┴─────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────────────┐
│                      Serialization & Data Types Layer                       │
│         (MessagePack, Protobuf, sketches, DogStatsD encoding)               │
├─────────────────┬──────────────────┬──────────────┬────────────────────────┤
│ libdd-tinybytes │ libdd-trace-     │ libdd-       │ libdd-ddsketch        │
│ `libdd-tinybytes│ protobuf         │ sampling     │ `libdd-ddsketch/src/` │
│ /src/`          │ `libdd-trace-    │ `libdd-      │ libdd-dogstatsd-client│
│ (ByteStr,       │ protobuf/src/`   │ sampling/src/│ `libdd-dogstatsd-client
│ ByteVec)        │ (Message/span    │ `           │ /src/`                │
│ libdd-library-  │ definitions)     │             │                       │
│ config          │ libdd-trace-stats│             │                       │
│ `libdd-library- │ `libdd-trace-    │             │                       │
│ config/src/`    │ stats/src/`      │             │                       │
│ libdd-remote-   │ (Stats for       │             │                       │
│ config          │ spans)           │             │                       │
│ `libdd-remote-  │                  │             │                       │
│ config/src/`    │                  │             │                       │
└─────────────────┴──────────────────┴──────────────┴────────────────────────┘
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

**Overall:** Layered monorepo with domain-specific crates (profiling, crash tracking, telemetry) at the middle layer, domain-agnostic infrastructure (HTTP, crypto, types) at the base, and paired FFI crates for C/C++ exposure.

**Key Characteristics:**
- **No global state in libraries:** Pure function design except where necessary (connectors, TLS providers). Callers explicitly initialize what they need.
- **FFI safety:** All FFI entry points deny panics, unwrap, and expect. Error returns use `Result` wrappers. Panics across FFI boundaries are caught with `catch_unwind`.
- **Feature-gated domains:** The builder selects which domains to compile (e.g., `crashtracker`, `profiling`, `telemetry`) to minimize binary size.
- **Async-first (Tokio):** Most I/O uses async/await with Tokio runtime, but keeps the Rust APIs synchronous where possible to simplify FFI.
- **Error types:** Structured error enums (via `thiserror`) bubble up through layers; FFI crates convert them to C-compatible status codes/strings.

## Layers

**FFI Layer:**
- Purpose: Expose Rust functionality to C/C++ callers via C ABI with struct/enum marshaling, opaque handle pointers, and generated headers.
- Location: `libdd-profiling-ffi/`, `libdd-crashtracker-ffi/`, `libdd-telemetry-ffi/`, `libdd-data-pipeline-ffi/`, `datadog-sidecar-ffi/`, etc.
- Contains: `#[repr(C)]` types, C function signatures, handle wrappers, conversion from Rust types to C-compatible representations.
- Depends on: Corresponding domain crates (libdd-profiling, libdd-crashtracker, etc.) + libdd-common-ffi for FFI primitives.
- Used by: C/C++ SDKs (via generated headers from cbindgen).

**Domain Implementation Layer:**
- Purpose: Implement concrete logic for profiling, crash tracking, telemetry, data routing, etc.
- Location: `libdd-profiling/`, `libdd-crashtracker/`, `libdd-telemetry/`, `libdd-data-pipeline/`, `datadog-sidecar/`, etc.
- Contains: Rust-native APIs, data collectors, state machines, async coordination, integration with lower-level utilities.
- Depends on: Shared infrastructure (libdd-common, libdd-trace-utils, serialization crates), platform-specific modules for Windows/Unix.
- Used by: Domain FFI crates + other domain crates (e.g., sidecar uses all domains).

**Shared Infrastructure Layer:**
- Purpose: Provide HTTP transport, TLS/crypto, serialization, error handling, tag validation, rate limiting, platform abstraction.
- Location: `libdd-common/`, `libdd-http-client/`, `libdd-trace-utils/`, `libdd-common-ffi/`, `libdd-capabilities*`, serialization crates.
- Contains: Connectors (reqwest/hyper backends, HTTPS with ring or FIPS crypto), platform APIs (Unix signals, Windows APIs), test utilities.
- Depends on: External crates (tokio, serde, prost, rustls, hyper, ring/aws-lc-rs).
- Used by: All domain crates.

**Serialization & Data Types Layer:**
- Purpose: Define data encodings (MessagePack, Protobuf), efficient byte representations, sampling rules, config structures.
- Location: `libdd-tinybytes/`, `libdd-trace-protobuf/`, `libdd-sampling/`, `libdd-ddsketch/`, `libdd-library-config/`, etc.
- Contains: Serde-derived structs, Protobuf definitions (compiled via prost), sketches, enum variants for config.
- Depends on: serde, prost, rmp-serde, base64, etc.
- Used by: All layers above.

## Data Flow

### Primary Request Path: Span Submission (Traces)

1. **Span ingestion** — Language SDK calls FFI function in `libdd-profiling-ffi` or `datadog-sidecar-ffi` to submit a span
2. **Marshaling** — FFI layer (`libdd-data-pipeline-ffi/src/`) converts C structs to Rust types
3. **Span normalization** — `libdd-trace-normalization` removes invalid tags, applies naming conventions (`libdd-trace-normalization/src/`)
4. **Span obfuscation** — `libdd-trace-obfuscation` scrubs PII and secrets (`libdd-trace-obfuscation/src/`)
5. **Routing decision** — `datadog-sidecar/src/work/` routes spans to aggregation tasks based on service/env
6. **Batching & buffering** — `libdd-data-pipeline/src/` collects spans into MessagePack-encoded payloads
7. **HTTP transport** — `libdd-trace-utils/src/transport/` batches payloads, retries, and sends via `libdd-http-client` to agent or Datadog API
8. **Agent submission** — `libdd-agent-client/src/` or direct API call via `libdd-common/src/connector/`

### Crash Collection Path

1. **Signal delivery** — OS delivers signal to crashing process; `libdd-crashtracker/src/` handler catches it (`libdd-crashtracker/src/runtime_callback.rs`)
2. **Crash data collection** — `libdd-crashtracker/src/crash_info/` gathers stack traces, register state, memory maps, process metadata
3. **Demangle symbols** — `libdd-crashtracker-ffi/src/demangler/` resolves and formats C++ symbols
4. **IPC send** — Payload marshaled and sent to sidecar via `datadog-ipc/src/`
5. **Sidecar processing** — `datadog-sidecar/src/` receives, enqueues crash data, batches and sends to backend

### Profile Submission Path

1. **Profile collection** — Native profiler (e.g., cprofile in Python) or `libdd-profiling/src/api/` collects CPU/heap samples
2. **Profile encoding** — `libdd-profiling/src/exporter/` or `libdd-profiling-ffi` encodes to pprof (protobuf) format
3. **HTTP transport** — Same as spans: batch, retry, send via `libdd-http-client`

**State Management:**
- **Buffering:** Spans and profiles buffered in memory via `libdd-data-pipeline/src/buffering/` pending HTTP submission.
- **Deduplication:** Sidecar applies dedup logic to reduce redundant spans.
- **Sidecar coordination:** `datadog-sidecar/src/` maintains async task queues (Tokio channels) for each domain; work items are pulled by submission tasks.

## Key Abstractions

**Handle Wrapper:**
- Purpose: Opaque pointer type for FFI, prevents accidental access to Rust objects from C code.
- Examples: `libdd-common-ffi/src/handle.rs`, `libdd-profiling-ffi/src/arc_handle.rs`
- Pattern: `struct DdProf<T>(*mut T)` with `#[repr(transparent)]` to ensure FFI compatibility.

**Result & Error Conversion:**
- Purpose: Convert Rust `Result<T>` to C-compatible `DdProfError` or status codes.
- Examples: `libdd-common-ffi/src/result.rs`, `libdd-profiling-ffi/src/profile_error.rs`
- Pattern: FFI functions return `DdProfError`, callers check `.is_ok()` or inspect error details.

**Slice & Vec Wrappers:**
- Purpose: Safe FFI ownership of arrays and dynamic vecs.
- Examples: `libdd-common-ffi/src/slice.rs`, `libdd-common-ffi/src/vec.rs`
- Pattern: `Slice<T>` for borrowed arrays (ptr + len), `Vec<T>` for owned dynamic vecs with FFI-safe lifetime management.

**CStr Wrapper:**
- Purpose: Safe C string ownership and UTF-8 validation.
- Examples: `libdd-common-ffi/src/cstr.rs`
- Pattern: `CStr` validated at boundaries, auto-dropped when returned from Rust.

**IPC Message Types:**
- Purpose: Efficient serialization of sidecar work items.
- Examples: `datadog-ipc/src/`, `datadog-ipc-macros/src/`
- Pattern: Define message structs with `#[ipc(..)]` macro, serialized via bincode or MessagePack.

**Capability Flags:**
- Purpose: Feature detection and conditional logic without runtime overhead.
- Examples: `libdd-capabilities/src/`
- Pattern: Thread-safe enum of capability states; allows graceful degradation when features unavailable.

## Entry Points

**libdd-profiling-ffi:**
- Location: `libdd-profiling-ffi/src/lib.rs` (FFI functions) + `libdd-profiling-ffi/src/arc_handle.rs` (handle wrappers)
- Triggers: Language SDK calls C functions (e.g., `ddog_prof_...`)
- Responsibilities: Accept profiles from native code, manage lifecycle, expose interning APIs, export profiles, manage exporters.

**libdd-crashtracker-ffi (Unix):**
- Location: `libdd-crashtracker-ffi/src/collector.rs`
- Triggers: Installed as signal handler via `ddog_crasht_init()`
- Responsibilities: Intercept SIGSEGV/SIGABRT/SIGBUS/etc., collect crash data, serialize and submit.

**libdd-crashtracker-ffi (Windows):**
- Location: `libdd-crashtracker-ffi/src/collector_windows/api.rs` (`ddog_crasht_init_windows`)
- Triggers: Installed by SDK at runtime
- Responsibilities: Hook Windows exception handler, collect unhandled exception data.

**datadog-sidecar:**
- Location: `datadog-sidecar/src/main.rs` (or as library via `datadog-sidecar/src/lib.rs`)
- Triggers: Spawned by language SDK as separate process or linked as library
- Responsibilities: Central hub for span routing, metric aggregation, remote config polling, feature flag evaluation, dynamic configuration.

**datadog-sidecar-ffi:**
- Location: `datadog-sidecar-ffi/src/lib.rs`
- Triggers: Language SDK calls via IPC
- Responsibilities: Span submission (minimal interface, mostly IPC bridging).

**libdd-telemetry-ffi:**
- Location: `libdd-telemetry-ffi/src/lib.rs`
- Triggers: Language SDK calls telemetry functions
- Responsibilities: Collect and submit observability telemetry.

**libdd-library-config-ffi:**
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

**What happens:** FFI calls or domain functions call `.block_on()` within Tokio tasks or use `std::thread::spawn()` without caution.
**Why it's wrong:** Blocks Tokio worker threads, starves other async tasks, causes latency spikes and potential deadlocks in high-concurrency scenarios.
**Do this instead:** Use async-first design (`async/await` in domain crates). For synchronous FFI, avoid spawning tasks that block the runtime. Wrap blocking calls in `tokio::task::spawn_blocking()` if necessary.

### Unwrap/Panic Outside Tests

**What happens:** Code uses `.unwrap()`, `.expect()`, or `panic!()` in non-test crate code.
**Why it's wrong:** FFI may propagate panics into C code, causing undefined behavior or crashes in language runtimes.
**Do this instead:** Return `Result<T, E>` or use `anyhow::bail!()` to bubble errors. Convert to C status codes at FFI boundary.

### Global Mutable State

**What happens:** Module-level `static mut` or `lazy_static` holding mutable state without synchronization.
**Why it's wrong:** Race conditions, fork-safety issues in forking environments (PHP-FPM, etc.), difficult to test.
**Do this instead:** Pass configuration/state explicitly as function arguments or wrap in `Arc<Mutex<T>>` or `Arc<RwLock<T>>` for shared state. Use thread-local for thread-scoped state.

### Ignoring Fork Safety

**What happens:** Code holds locks or file descriptors that become inconsistent after `fork()`.
**Why it's wrong:** Forking processes (PHP-FPM, Apache, multiprocessing Python) crash or deadlock with locked resources.
**Do this instead:** Register fork handlers via `libc::pthread_atfork()` (wrapped in `libdd-shared-runtime-ffi`) or accept explicit post-fork callbacks to reinitialize.

### Assuming Synchronous Behavior

**What happens:** FFI caller assumes that calling an async Rust function will complete synchronously.
**Why it's wrong:** Async functions return immediately (with a future); work is enqueued on Tokio runtime, causing ordering violations.
**Do this instead:** Document async behavior clearly in FFI. Provide explicit submission + polling/callback APIs, or wrap async logic in FFI-safe synchronous wrapper.

## Error Handling

**Strategy:** Structured error types (via `thiserror`) throughout domain crates; conversion to C-compatible status codes and error messages at FFI boundaries.

**Patterns:**
- **Domain crates:** Use `Result<T, DomainError>` where `DomainError` is an enum variant or `anyhow::Error`.
- **FFI crates:** Convert to `DdProfError` or status code; return error details via out-parameters or error string accessors.
- **Panic safety:** FFI entry points wrap Rust logic in `std::panic::catch_unwind()`, convert panics to `DdProfError::Internal`.

## Cross-Cutting Concerns

**Logging:** Optional tracing crate integration (feature-gated). Sidecar can enable structured logs via `tracing-subscriber`. FFI doesn't expose logging directly.

**Validation:** Input validation at FFI boundaries (e.g., valid UTF-8 for CStr, non-null pointers for slices). Domain crates assume validated inputs.

**Authentication:** Not handled directly; relies on caller (agent/API endpoint) for TLS/mTLS. libdd-common provides connector setup; no token/key logic in libraries.

---

*Architecture analysis: 2026-06-15*
