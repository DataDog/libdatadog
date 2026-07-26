# Testing Patterns

**Analysis Date:** 2026-06-15

## Test Framework

**Runner:**
- `cargo nextest` (preferred for workspace)
- `cargo test` (traditional, used for doc tests)
- Configured in `.config/nextest.toml` at repo root

**Assertion Library:**
- Standard `assert!()` macros
- `tokio::test` for async unit tests
- Pattern matching in assertions: `assert!(matches!(result, Err(HttpClientError::TimedOut)))`

**Run Commands:**
```bash
cargo nextest run --workspace --no-fail-fast         # Full workspace test run
cargo nextest run -p <crate-name>                    # Single crate
cargo nextest run -p <crate-name> <test-name>        # Single test by substring
cargo nextest run --workspace --all-features         # With all features enabled
cargo nextest run -E '!test(tracing_integration_tests::)'  # Exclude pattern
cargo test --doc                                     # Run doc tests only
```

**Nextest Configuration** (`.config/nextest.toml`):
- Experimental features: setup scripts
- Pre-build script for bin_tests: `cargo run -p bin_tests --bin prebuild`
- Store directory: `target/nextest`
- Single-threaded test group for `::single_threaded_tests::`
- Default profile: fail-fast on first failure, show skip/pass/slow/fail status
- CI profile: no fail-fast, generate JUnit XML report
- JUnit output: `junit.xml` in store directory

## Test File Organization

**Location:**
- **Unit tests:** Co-located in source file via `#[cfg(test)]` module (same file as code)
- **Integration tests:** Separate `.rs` files in `tests/` directory at crate root
- **Doc tests:** Embedded in doc comments with ` ```rust ` code blocks

**Naming:**
- Unit test functions: `test_<description>()` (e.g., `test_request_times_out()`)
- Integration test files: `<feature>_test.rs` (e.g., `timeout_test.rs`, `retry_test.rs`)
- Common test utilities: `tests/common.rs` or `common` module

**Structure:**
```
libdd-http-client/
├── src/
│   ├── lib.rs          # Public API
│   ├── client.rs       # Includes #[cfg(test)] mod tests { ... }
│   ├── error.rs        # Includes error display tests
│   └── config.rs       # Includes builder tests
└── tests/
    ├── timeout_test.rs
    ├── retry_test.rs
    ├── http_round_trip.rs
    ├── uds_round_trip.rs
    ├── connection_pool.rs
    └── common.rs       # Shared test utilities
```

## Test Structure

**Unit Test Pattern** (inline in source):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    
    #[test]
    fn new_creates_client() {
        ensure_crypto_provider();
        let client = HttpClient::new("http://localhost:8126".to_owned(), Duration::from_secs(3));
        assert!(client.is_ok());
        let client = client.unwrap();
        assert_eq!(client.config().base_url(), "http://localhost:8126");
    }
}
```

**Async Test Pattern** (with tokio::test):
```rust
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn test_request_times_out() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;
    
    server.mock_async(|when, then| {
        when.method(GET).path("/slow");
        then.status(200).delay(Duration::from_secs(10));
    }).await;
    
    let client = HttpClient::new(server.url("/"), Duration::from_millis(200)).unwrap();
    let req = HttpRequest::new(HttpMethod::Get, server.url("/slow"));
    let result = client.send(req).await;
    
    assert!(matches!(result, Err(HttpClientError::TimedOut)));
}
```

**Patterns:**
- Setup functions extracted: `ensure_crypto_provider()` called at test start
- Mocking via `httpmock::prelude::*` with fluent builder interface
- Assertions use `matches!()` for enum pattern matching on error types
- Test attributes: `#[test]`, `#[tokio::test]`, `#[cfg_attr(miri, ignore)]`

## Mocking

**Framework:** `httpmock` crate

**Patterns:**
```rust
use httpmock::prelude::*;

// Synchronous server
let server = MockServer::start();
let mock = server.mock(|when, then| {
    when.method(PUT).path("/v0.5/traces").header("X-Datadog-Trace-Count", "42");
    then.status(200).body(r#"{"rate_by_service":{}}"#);
});

// Async server
let server = MockServer::start_async().await;
let mock = server.mock_async(|when, then| {
    when.method(GET).path("/slow");
    then.status(200).delay(Duration::from_secs(10));
}).await;

// Assert mock was called with specific count
mock.assert();           // Called at least once
mock.assert_calls_async(3).await;  // Called exactly 3 times
```

**What to Mock:**
- HTTP servers (for integration tests that don't need real server)
- External service responses (when testing retry/error handling)
- Timeouts and network conditions (for resilience testing)

**What NOT to Mock:**
- Cryptographic primitives (always use real crypto)
- Serialization/deserialization (test with actual encoded data)
- Internal HTTP layer (test actual reqwest/hyper behavior)

## Fixtures and Factories

**Test Data Pattern:**
```rust
// From libdd-agent-client/tests/common.rs
pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn client_for(server: &MockServer) -> AgentClient {
    ensure_crypto_provider();
    AgentClient::builder()
        .http("localhost", server.port())
        .language_metadata(LanguageMetadata::new(
            "python", "3.12.1", "CPython", "", "2.18.0",
        ))
        .build()
        .expect("client build failed")
}
```

**Location:**
- Shared fixtures in `tests/common.rs` (imported by test files)
- Factory functions for commonly used test objects
- Setup helpers extracted into functions for reuse

## Coverage

**Requirements:** Not enforced; target is high coverage through test organization

**View Coverage:**
```bash
# Using tarpaulin or llvm-cov (if installed)
cargo tarpaulin --workspace
cargo llvm-cov --workspace
```

**Coverage Focus:**
- Public API paths (especially error cases)
- Retry logic and timing-sensitive code
- FFI boundary safety (panic catching, error conversion)

## Test Types

**Unit Tests:**
- Scope: Single function or method
- Approach: Synchronous, inline in source file via `#[cfg(test)]`
- Example: Error type display formatting in `libdd-http-client/src/error.rs`
  ```rust
  #[test]
  fn connection_failed_display() {
      let err = HttpClientError::ConnectionFailed("refused".to_owned());
      assert_eq!(err.to_string(), "connection failed: refused");
  }
  ```

**Integration Tests:**
- Scope: Multiple components, HTTP client behavior end-to-end
- Approach: Async with tokio::test, use real mock server (httpmock)
- Location: `tests/` directory
- Examples: `timeout_test.rs`, `retry_test.rs`, `http_round_trip.rs`

**Doc Tests:**
- Scope: Code examples in public API documentation
- Approach: Embedded in doc comments with ` ```rust ` blocks
- Run via: `cargo test --doc`
- Example from `libdd-http-client/src/lib.rs`:
  ```rust
  /// # Quick start
  /// ```rust,no_run
  /// # async fn example() -> Result<(), libdd_http_client::HttpClientError> {
  /// use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
  /// # Ok(())
  /// # }
  /// ```
  ```

**Special Test Patterns:**

- **Miri tests** (memory interpreter safety checks): Marked with `#[cfg_attr(miri, ignore)]` to skip in miri runs (e.g., network I/O can't run under miri)
- **FFI tests** (`cargo ffi-test`): Runs C/C++ FFI examples from `libdd-*-ffi` crates
- **Feature-gated tests** (`--all-features`): Crates with multiple backends tested independently
  ```bash
  # Default (reqwest) backend
  cargo nextest run -p libdd-http-client
  # Hyper backend (must be tested separately)
  cargo nextest run -p libdd-http-client --no-default-features --features hyper-backend,https
  ```
- **Spawn_from_lib tests** (thread spawning safety): Requires feature flag
  ```bash
  cargo nextest run --package test_spawn_from_lib --features prefer-dynamic
  ```
- **Tracing integration tests** (Docker-dependent): Skip locally if Docker unavailable
  ```bash
  cargo nextest run -E '!test(tracing_integration_tests::)'
  ```
- **Crashtracker tests** (unit test file generation): Requires feature flag
  ```bash
  cargo nextest run --features libdd-crashtracker/generate-unit-test-files
  ```

## Async Testing

**Pattern:**
```rust
#[tokio::test]
async fn test_request_times_out() {
    // Setup
    let server = MockServer::start_async().await;
    
    // Mock setup
    server.mock_async(|when, then| {
        when.method(GET).path("/slow");
        then.status(200).delay(Duration::from_secs(10));
    }).await;
    
    // Test execution
    let client = HttpClient::new(server.url("/"), Duration::from_millis(200)).unwrap();
    let result = client.send(req).await;
    
    // Assertions
    assert!(matches!(result, Err(HttpClientError::TimedOut)));
}
```

**Key points:**
- `#[tokio::test]` instead of `#[test]` for async functions
- Mock server `.start_async()` and mock setup `.await`
- `client.send()` is awaited (async I/O)
- Test function itself is `async fn`

## Error Testing

**Pattern:**
```rust
#[test]
fn error_display_includes_status() {
    let err = HttpClientError::RequestFailed {
        status: 503,
        body: "service unavailable".to_owned(),
    };
    assert_eq!(
        err.to_string(),
        "request failed with status 503: service unavailable"
    );
}

#[tokio::test]
async fn test_retries_on_503() {
    let result = client.send(req).await;
    assert!(matches!(result, Err(HttpClientError::RequestFailed { status: 503, .. })));
}
```

**Patterns:**
- Test error variant construction and display messages
- Use `matches!()` to assert on specific error variants
- For retryable errors, verify retry count via mock assertion
- For non-retryable errors (e.g., InvalidConfig), verify no retries occur

## Common Test Setup

**Crypto Provider Initialization** (required for TLS tests):
```rust
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::test]
async fn test_foo() {
    ensure_crypto_provider();
    // ... test body
}
```

## Test Command Reference

**Standard validation workflow** (from AGENTS.md):
```bash
# 1. Check single crate during iteration
cargo check -p <crate>

# 2. Format and lint before finishing
cargo +nightly-2026-02-08 fmt --all -- --check
cargo +stable clippy --workspace --all-targets --all-features -- -D warnings

# 3. Run tests
cargo nextest run --workspace --no-fail-fast
cargo nextest run --workspace --all-features --exclude builder --exclude test_spawn_from_lib
cargo test --doc

# 4. FFI tests (if FFI crates touched)
cargo ffi-test

# 5. Tracing integration tests (if Docker available)
# Otherwise skip with: -E '!test(tracing_integration_tests::)'

# 6. Verify licenses (if Cargo.lock touched)
./scripts/update_license_3rdparty.sh
cargo deny check
```

---

*Testing analysis: 2026-06-15*
