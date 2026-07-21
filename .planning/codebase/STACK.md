# Technology Stack

**Analysis Date:** 2026-06-15

## Languages

**Primary:**
- Rust 1.87.0 - Core implementation language for all workspace crates, FFI bindings, and shared libraries

**Secondary:**
- C/C++ - FFI consumers and examples (via cbindgen-generated headers)
- Protobuf - Data serialization format (compiled to Rust via prost)

## Runtime

**Environment:**
- tokio 1.23+ (async runtime for networking, multithreading support)
- System native threading and IPC (Unix domain sockets, Windows named pipes)

**Package Manager:**
- cargo (Rust package manager)
- Lockfile: `Cargo.lock` (present, committed)

## Frameworks

**Core:**
- tokio 1.23-1.49 - Async runtime for all async operations
- hyper 1.6 - HTTP/1.1 client and server framework
- prost 0.14.1 - Protocol buffers serialization (tracing and profiling data)
- reqwest 0.13 - HTTP client with rustls TLS (default backend)
- serde/serde_json 1.0 - Serialization/deserialization

**Async & IPC:**
- futures 0.3 - Async utilities and utilities for composing async code
- tokio-util 0.7 - Tokio utilities (codec, framing)
- manual_future 0.1.1 - Manual future composition
- crossbeam-queue 0.3 - Lock-free queue for IPC

**FFI & Build:**
- cbindgen 0.29 - C header generation from Rust code (feature-gated via `cbindgen` feature)
- cmake 0.1.50 - Build system for C/C++ examples and cross-compilation
- prost-build 0.14.1 - Protobuf code generation
- protoc-bin-vendored 3.0.0 - Vendored protoc compiler
- build-common (internal crate) - Shared build helpers

**Cryptography & TLS:**
- rustls 0.23 - TLS implementation (no provider by default)
- rustls with ring provider - Default HTTPS: ring as crypto backend
- aws-lc-rs - FIPS-compliant crypto provider (via `fips` feature, Unix only)
- tokio-rustls 0.26 - Async TLS support via tokio
- hyper-rustls 0.27.7 - TLS support for hyper
- rustls-native-certs 0.8.1-0.8.2 - Native certificate store access
- rustls-platform-verifier 0.6 - Platform-specific certificate verification
- hickory-dns - DNS resolver (replaces system resolver for fork safety)

**Testing:**
- bolero 0.13 - Property-based fuzzing framework (feature-gated)
- httpmock 0.8.0-alpha.1 - HTTP mock server for testing
- tempfile 3.x - Temporary file management for tests
- serial_test 3.2 - Test serialization utilities

## Key Dependencies

**Critical:**
- anyhow 1.0 - Error handling with context
- thiserror 1.0-2.0 - Structured error types with `#[derive]` macros
- libc 0.2 - Bindings to system C library
- bytes 1.4 - Efficient byte buffer utilities for networking
- base64 0.22 - Base64 encoding/decoding

**Infrastructure & Serialization:**
- serde_json 1.0 - JSON serialization with raw value support
- serde_with 3.x - Additional serde helpers
- serde_bytes 0.11.9 - Efficient byte serialization
- serde_yaml 0.9.34 - YAML serialization
- uuid 1.3-1.7 - UUID generation (v4)
- chrono 0.4.31+ - DateTime handling with timezone support
- regex/regex-lite 1.5 - Pattern matching (lite variant for binary size reduction)
- hashbrown 0.15 - Hash map/set implementation

**Logging & Observability:**
- tracing 0.1 - Structured logging/tracing instrumentation
- tracing-subscriber 0.3.22 - Tracing configuration and output
- tracing-log 0.2.0 - Bridge from tracing to legacy log crate
- tracing-appender 0.2.3 - Rotating file appenders for logs
- console-subscriber 0.5 - tokio-console task introspection (feature-gated)

**System Utilities:**
- sys-info 0.9.0 - OS information (Windows/Unix)
- memory-stats 1.2.0 - Memory usage statistics with statm support
- prctl 1.0.0 - Process control (Linux)
- nix 0.29 - Safe POSIX system call bindings (Unix)
- windows/windows-sys 0.51-0.59 - Windows API bindings

**Protocol & Demangle:**
- symbolic-demangle 12.8.0 - Stack frame demangling (Rust, C++, MSVC)
- symbolic-common 12.8.0 - Symbolic debugging utilities
- cadence 1.3.0 - DogStatsD client library

**Build & CLI:**
- pico-args 0.5.0 - Lightweight CLI argument parsing
- toml 0.8.19 - TOML parsing/serialization
- cmake 0.1.50 - CMake build system integration
- tar 0.4.45 - TAR archive handling

**FFI & Unsafe Code:**
- function_name 0.3.0 - Get current function name at compile time
- paste 1.0 - Macro paste helper for code generation
- allocator-api2 0.2.21 - Allocator traits
- const_format 0.2.34 - Const string formatting

**Specialized:**
- flate2 1.0 - gzip/deflate compression
- simd-json 0.14-0.15 - SIMD-accelerated JSON parsing (non-x86 arch)
- rmp-serde 1.3.0 - MessagePack serialization (sidecar IPC)
- bincode 1.3.3 - Binary serialization format
- sha2 0.10 - SHA2 hashing
- zwohash 0.1.2 - Hash function for fast hashing

## Configuration

**Environment:**
- Configuration via environment variables:
  - `DD_TRACE_AGENT_URL` - Agent endpoint
  - `DD_AGENT_HOST` - Agent hostname (default: localhost)
  - `DD_TRACE_AGENT_PORT` - Agent port (default: 8126)
  - `DD_API_KEY` - Datadog API key
  - `DD_SITE` - Datadog site (default: datadoghq.com)
  - `_DD_DIRECT_SUBMISSION_ENABLED` - Direct submission to Datadog intake
  - `DD_TELEMETRY_HEARTBEAT_INTERVAL` - Telemetry heartbeat frequency
  - `DD_APM_TELEMETRY_DD_URL` - Custom telemetry endpoint
  - Internal: `_DD_DEBUG_*`, `_DD_SIDECAR_*` for debugging/sidecar configuration

**Build:**
- `Cargo.toml` workspace manifest with feature flags for:
  - `https` - TLS support via rustls + ring (default)
  - `fips` - FIPS-compliant crypto via aws-lc-rs (Unix only)
  - `reqwest-backend` - Reqwest HTTP client (default)
  - `hyper-backend` - Hyper HTTP client (alternative)
  - Feature flags per-crate for optional functionality (protobuf generation, fuzzing, etc.)

**Tooling Config:**
- `rust-toolchain.toml` - Pinned Rust 1.87.0 with rustfmt and clippy
- `.cargo/config.toml` - Cargo aliases (e.g., `ffi-test`)
- `rustfmt.toml` - Code formatting rules
- `clippy.toml` - Linter configuration
- `.config/nextest.toml` - Test runner configuration
- `deny.toml` - Dependency audit configuration (multiple versions warning)

## Platform Requirements

**Development:**
- Rust 1.87.0 (or newer per MSRV)
- cargo with workspace resolver v2
- cbindgen 0.29 (for FFI header generation)
- cmake 3.x (for C/C++ example builds)
- protoc (protobuf compiler) - can use vendored version via feature
- System C compiler (gcc/clang on Unix, MSVC on Windows)

**Build Constraints:**
- Rust version must be compatible with:
  - Alpine Linux latest
  - RHEL 8.x and 9.x (via community packaging)
- FIPS feature requires `AWS_LC_FIPS_SYS_NO_ASM=1` on Windows
- Nextest 0.9.96 for test execution

**Production:**
- Deployment as shared library (dylib, staticlib, or cdylib)
- Requires Datadog agent (default: localhost:8126) or direct API key for agentless submission
- Optional Docker for integration tests (`tracing_integration_tests`)

---

*Stack analysis: 2026-06-15*
