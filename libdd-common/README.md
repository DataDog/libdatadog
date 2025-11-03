# libdd-common

Common utilities and foundational components shared across Datadog telemetry libraries.

## Overview

`libdd-common` provides shared functionality used across multiple Datadog Rust libraries including HTTP client support, entity ID detection, tag handling, and platform-specific utilities.

## Features

- **HTTP/HTTPS Client**: Async HTTP client built on hyper with rustls support
- **Entity ID Detection**: Automatic container ID and entity ID detection from cgroups, Docker, Kubernetes, and Azure App Services
- **Tag Management**: Compile-time validated tags with the `tag!` macro
- **URI Parsing**: Special handling for Unix sockets, Windows named pipes, and file URIs  
- **Rate Limiting**: Token bucket rate limiter implementation
- **Platform Utilities**: Unix-specific utilities for fork, exec, and process management
- **Datadog Headers**: Standard Datadog HTTP headers and constants
- **Timeout Support**: Configurable timeouts for HTTP requests
- **Worker Abstraction**: Generic worker pattern for background tasks

## Modules

- `azure_app_services`: Azure App Services integration
- `config`: Configuration utilities
- `connector`: HTTP/HTTPS connector implementations
- `cstr`: C string utilities and macros
- `entity_id`: Container and entity ID detection
- `error`: Common error types
- `header`: Datadog HTTP headers
- `hyper_migration`: Hyper version migration helpers
- `rate_limiter`: Rate limiting implementation
- `tag`: Tag creation and validation
- `timeout`: Timeout utilities
- `unix_utils`: Unix-specific process utilities
- `worker`: Background worker abstraction

## Examples

### Creating tags

```rust
use libdd_common::tag;

// Compile-time validated tag
let tag1 = tag!("service", "my-service");

// Runtime tag creation
use libdd_common::tag::Tag;
let tag2 = Tag::new("env", "production")?;
```

### Entity ID detection

```rust
use libdd_common::entity_id;

if let Some(container_id) = entity_id::get_container_id() {
    println!("Running in container: {}", container_id);
}
```

## Features Flags

- `https` (default): Enable HTTPS support with rustls
- `use_webpki_roots`: Use webpki roots instead of native certs
- `cgroup_testing`: Enable cgroup stubbing for testing
- `fips`: Use FIPS-compliant cryptographic provider (Unix only)

