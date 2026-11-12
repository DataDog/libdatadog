# libdd-http-client

HTTP client library for use by external language libraries via FFI. Not intended for use within libdatadog.

## Overview

`libdd-http-client` provides a simple async HTTP client with connection pooling, configurable retry, and platform-specific transport support. It offers a hyper backend (the default) and a reqwest backend behind a stable API surface designed for FFI consumption.

## Quick Start

```rust
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;

let client = HttpClient::new(
    "http://localhost:8126".to_owned(),
    Duration::from_secs(5),
)?;

let request = HttpRequest::new(
    HttpMethod::Post,
    "http://localhost:8126/v0.4/traces".to_owned(),
);
let response = client.send(request).await?;
println!("Status: {}", response.status_code());
```

## Builder API

Use the builder for advanced configuration:

```rust
use libdd_http_client::{HttpClient, RetryConfig};
use std::time::Duration;

let client = HttpClient::builder()
    .base_url("http://localhost:8126".to_owned())
    .timeout(Duration::from_secs(5))
    .treat_http_errors_as_errors(false)
    .retry(RetryConfig::new().max_retries(3))
    .build()?;
```

## Transports

### TCP (default)

HTTP and HTTPS over TCP. HTTPS is enabled by default via the `https` feature.

### Unix Domain Socket

```rust
let client = HttpClient::builder()
    .base_url("http://localhost".to_owned())
    .timeout(Duration::from_secs(5))
    .unix_socket("/var/run/datadog/apm.socket")
    .build()?;
```

### Windows Named Pipe

```rust
let client = HttpClient::builder()
    .base_url("http://localhost".to_owned())
    .timeout(Duration::from_secs(5))
    .windows_named_pipe(r"\\.\pipe\dd_agent")
    .build()?;
```

## Retry

Retry is opt-in. When enabled, all errors except `InvalidConfig` are retried with exponential backoff and jitter.

```rust
use libdd_http_client::RetryConfig;

let retry = RetryConfig::new()
    .max_retries(3)           // default: 3
    .initial_delay(Duration::from_millis(100))  // default: 100ms
    .with_jitter(true);       // default: true
```

Backoff doubles each attempt: 100ms, 200ms, 400ms, etc.

## Redirects

Redirects are not followed: a 3xx response is returned as it is. Following one resends the request,
including its body and custom headers such as credentials, to the redirect target.

```rust
let client = HttpClient::builder()
    .base_url("http://localhost:8126".to_owned())
    .timeout(Duration::from_secs(5))
    .follow_redirects(true) // reqwest backend only
    .build()?;
```

With the hyper backend, `follow_redirects(true)` makes `build()` return `InvalidConfig`.

## Multipart Uploads

Send multipart/form-data requests by adding parts to the request. Content-Type
and boundary are handled automatically.

```rust
use libdd_http_client::{HttpMethod, HttpRequest, MultipartPart};

let mut req = HttpRequest::new(HttpMethod::Post, url);
req.add_multipart_part(
    MultipartPart::new("metadata", json_bytes)
        .content_type("application/json"),
);
req.add_multipart_part(
    MultipartPart::new("file", binary_data)
        .filename("archive.zip")
        .content_type("application/octet-stream"),
);
```

Setting both multipart parts and a body on the same request returns an error.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `https` | yes | HTTPS via rustls with ring. The hyper backend installs the provider itself; with the reqwest backend, install one before building a client |
| `hyper-backend` | yes | hyper-based backend via libdd-common, resolving names with the system resolver |
| `reqwest-backend` | no | reqwest-based backend, resolving names with the in-process hickory-dns resolver |
| `hyper-proxy` | no | Honor `HTTPS_PROXY`/`NO_PROXY` in the hyper backend |
| `fips` | no | FIPS-compliant TLS via rustls without default crypto provider |

Enable exactly one of `hyper-backend` or `reqwest-backend`. To use reqwest:

```toml
libdd-http-client = { version = "...", default-features = false, features = ["https", "reqwest-backend"] }
```

If both are enabled, reqwest is used, so adding `reqwest-backend` to the default features also
selects it. The hyper backend does not pull reqwest into the build.

### FIPS TLS

When using the `fips` feature, call `init_fips_crypto()` once at startup before constructing any client:

```rust
libdd_http_client::init_fips_crypto()
    .expect("failed to install FIPS crypto provider");
```

Do not enable `fips` and `https` simultaneously — `https` pulls in a non-FIPS crypto provider.

## Error Handling

`HttpClientError` variants:

| Variant | Retried | Description |
|---------|---------|-------------|
| `RequestFailed { status, body }` | yes | HTTP 4xx/5xx (when `treat_http_errors_as_errors` is true) |
| `ConnectionFailed(msg)` | yes | TCP/socket connection failure |
| `IoError(msg)` | yes | I/O error during request |
| `TimedOut` | yes | Request exceeded timeout |
| `InvalidConfig(msg)` | no | Client misconfiguration |
