# Coding Conventions

**Analysis Date:** 2026-06-15

## Naming Patterns

**Files:**
- Snake case: `libdd_http_client`, `libdd_trace_utils`, `span_utils.rs`
- FFI crate suffix: `-ffi` (e.g., `libdd-common-ffi`, `libdd-http-client` exposes FFI via separate `-ffi` crates)
- Module files match module names: `client.rs`, `error.rs`, `config.rs`, `retry.rs`, `request.rs`, `response.rs`

**Functions:**
- Snake case: `ensure_crypto_provider()`, `send_traces()`, `send_once()`, `handle_panic_error()`
- Private helper functions prefixed with underscore when needed (e.g., module-private: `fn from_config_and_transport()`)
- Builder methods use chainable names: `base_url()`, `timeout()`, `with_filename()`, `build()`
- Async functions clearly marked: `async fn send()`, `async fn send_with_retry()`
- Getter methods omit `get_` prefix: `config()`, `timeout()`, `retry()` (not `get_config()`)

**Variables:**
- Snake case: `base_url`, `retry_config`, `mock_server`, `last_err`, `crypto_provider`
- Field names in structs: snake case (e.g., `treat_http_errors_as_errors: bool`)
- Loop variables conventional: `attempt`, `err`, `delay`

**Types:**
- PascalCase for structs and enums: `HttpClient`, `HttpRequest`, `HttpClientError`, `HttpMethod`, `MultipartPart`
- Error variants as concrete enum members: `HttpClientError::TimedOut`, `HttpClientError::ConnectionFailed(String)`
- Config types: `HttpClientConfig`, `RetryConfig`, `HttpClientBuilder`

**Macros:**
- All caps with underscores: `wrap_with_ffi_result!`, `wrap_with_void_ffi_result!`, `wrap_with_ffi_result_no_catch!`
- Decorated with `#[named]` attribute to capture function name for error reporting

## Code Style

**Formatting:**
- Tool: `rustfmt` (nightly-2026-02-08)
- Config: `rustfmt.toml` at repo root
  - Line width: 100 characters (max_width, comment_width, doc_comment_code_block_width)
  - Format macro matchers enabled
  - Format code in doc comments enabled
  - Wrap comments enabled
  - Ignores: `datadog-ipc/tarpc/` (embedded upstream project)

**Linting:**
- Tool: `clippy` (stable)
- Config: `clippy.toml` at repo root
  - `max-struct-bools = 5` (allow up to 5 independent boolean fields in config structs)
  - `allow-unwrap-in-tests = true`
  - `allow-expect-in-tests = true`
  - `allow-panic-in-tests = true`

**Compiler Lint Attributes** (standard across all crates):
Applied in `lib.rs` of each crate via `#![cfg_attr(...)]`:

```rust
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
```

- **Production code must not:**
  - Call `unwrap()`, `expect()`, `todo!()`, `unimplemented!()`, or panic
  - These are explicitly allowed in tests via clippy.toml
- **Exception:** `unwrap_or_else()` is acceptable for fallback error handling (e.g., `last_err.unwrap_or_else(|| HttpClientError::...)`), not flagged as `unwrap_used`
- **FFI entry points:** Must wrap with `catch_unwind` and `wrap_with_ffi_result!` macro

**Documentation:**
- All public items require doc comments via `#![deny(missing_docs)]`
- Doc comments explain the public API, not implementation details
- Examples show usage in doc comments when helpful
- Library modules document module-level purpose with module-level doc comments

## Import Organization

**Order:**
1. Standard library imports (`use std::...`)
2. External crate imports (third-party, alphabetically)
3. Crate-relative imports (`use crate::...`)
4. Module-relative imports (`use super::...`)

**Example from `libdd-http-client/src/client.rs`:**
```rust
use crate::backend::Backend;
use crate::config::{HttpClientBuilder, HttpClientConfig, TransportConfig};
use crate::{HttpClientError, HttpRequest, HttpResponse};
use std::time::Duration;
```

**Re-exports:**
- Barrel exports at crate root (`lib.rs`) expose public types:
  ```rust
  pub use client::HttpClient;
  pub use config::{HttpClientBuilder, HttpClientConfig};
  pub use error::HttpClientError;
  ```
- Private modules marked with `mod` (e.g., `mod client; mod error;`)
- Public modules marked with `pub mod` for re-export (e.g., `pub mod config; pub mod retry;`)

## Error Handling

**Strategy:** Structured error enums with `thiserror` crate

**Error Pattern:**
- Define enum with `#[derive(Debug, Error)]` from `thiserror`
- Each variant has error display message via `#[error(...)]` attribute
- Variants may contain structured data (e.g., status code, body text)

**Example from `libdd-http-client/src/error.rs`:**
```rust
#[derive(Debug, Error)]
pub enum HttpClientError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("request timed out")]
    TimedOut,
    
    #[error("request failed with status {status}: {body}")]
    RequestFailed { status: u16, body: String },
}
```

**Result Type Convention:**
- Use `Result<T, ErrorType>` (not `Option<T>`)
- Return results all the way up; catch/handle at boundaries only
- Bubble errors with context using `anyhow::Context` trait (`context()` method)

**FFI Error Conversion:**
- FFI crates define `Error` struct that wraps `Vec<u8>` (FFI-safe string buffer)
- Convert `anyhow::Error` to FFI `Error` via `From<anyhow::Error>` impl
- Handle panics in FFI entry points with `catch_unwind` and convert to error returns
- Never let panics propagate across FFI boundaries (undefined behavior)

**Example from `libdd-common-ffi/src/error.rs`:**
```rust
impl From<anyhow::Error> for Error {
    fn from(value: anyhow::Error) -> Self {
        // Use alternate format to include context chain
        Self::from(format!("{value:#}"))
    }
}
```

## Logging

**Framework:** `log` crate (or direct `println!` for simple cases)

**Patterns:**
- Avoid logging in hot paths (performance-critical sections)
- Library code typically does not log; let the caller control logging
- If logging is needed, use structured logging where possible
- No println! in production library code (stderr/stdout pollution)

## Comments

**When to Comment:**
- Explain *why*, not *what* (code shows what)
- Document non-obvious behavior, safety invariants, FFI considerations
- Mark platform-specific code: `#[cfg(unix)]`, `#[cfg(windows)]`
- Explain algorithm complexity or performance rationale
- Document panics/abort conditions in tests only

**JSDoc/TSDoc / RustDoc:**
- Required for all public items via `#![deny(missing_docs)]`
- Format: `/// Single-line summary` or multi-line with `///`
- Code examples in docs wrapped with ` ```rust ` and ` ``` `
- Use `#[example]` for longer runnable examples
- Safety invariants documented with `// Safety:` comments in unsafe blocks

**Example from `libdd-http-client/src/config.rs`:**
```rust
/// Create a config with the given base URL and timeout. HTTP errors are
/// treated as errors by default.
pub(crate) fn new(base_url: String, timeout: Duration) -> Self {
```

## Function Design

**Size:**
- Keep functions focused on a single responsibility
- Typical range: 20-50 lines for public functions; smaller for helpers
- Long async functions acceptable if clear control flow (e.g., retry loops)

**Parameters:**
- Use builder pattern for many parameters (e.g., `HttpClientBuilder`)
- Prefer `impl Into<T>` for string-like conversions: `name: impl Into<String>`
- Async functions return `async fn() -> Result<T, E>`

**Return Values:**
- Always use `Result<T, E>` (never `Option<Result<...>>`)
- Return early with `?` operator
- Chain methods on builders (consume self, return self)

**Example from `libdd-http-client/src/config.rs`:**
```rust
pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
    self.filename = Some(filename.into());
    self
}
```

## Module Design

**Exports:**
- Crate root (`lib.rs`) re-exports public API via `pub use`
- Module boundaries hide implementation (e.g., `backend/` is `pub(crate)`)
- Private modules grouped by feature or domain

**Barrel Files:**
- Crate root `lib.rs` acts as barrel file
- Does *not* re-export internal modules; only the public API types

**Module Organization Pattern:**
```
src/
├── lib.rs           # Public API re-exports, crate documentation
├── config.rs        # Public config structs
├── client.rs        # Public main client type
├── error.rs         # Public error type
├── request.rs       # Public request types
├── response.rs      # Public response types
├── retry.rs         # Public retry configuration
└── backend/         # Private backend implementation
    ├── mod.rs
    ├── reqwest_backend.rs
    └── hyper_backend.rs
```

**Public vs Private:**
- `pub mod config;` — re-exports module at crate root
- `mod backend;` — private implementation detail
- `pub(crate) fn from_config()` — internal to crate, not in public API

## Async/Await

**Pattern:**
- Use `tokio::test` for async unit tests: `#[tokio::test] async fn test_foo() { ... }`
- Use `tokio::spawn` when spawning tasks (rare in this codebase; prefer single-threaded)
- Never spawn threads in library code unless feature-gated; let the caller control concurrency
- Use `async fn` for all I/O-bound operations

**Example:**
```rust
pub async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
    self.backend.send(request, &self.config).await
}
```

## Concurrency & Globals

**No global state in library code:**
- No static mutable variables in production code
- Exception: `catch_unwind` in FFI entry points (macro handles safely)
- Exception: Feature-gated cryptographic provider initialization (caller responsible)
- Thread-safe via immutable references; no locks in hot paths

**FFI Crypto Provider Initialization:**
- Called once at startup: `libdd_http_client::init_fips_crypto()?`
- Returns error if provider already installed (safety check)
- Caller ensures single initialization

## Testing Patterns

- Tests can use `unwrap()`, `expect()`, `panic!()` (allowed by clippy.toml)
- Unit tests in `#[cfg(test)]` modules within source files
- Integration tests in `tests/` directory at crate root
- Async tests use `#[tokio::test]` attribute
- Doc tests run via `cargo test --doc`

---

*Conventions analysis: 2026-06-15*
