# libdd-common

Common utilities and foundational components shared across Datadog telemetry libraries.

## Overview

`libdd-common` provides shared functionality used across multiple Datadog Rust libraries including entity ID detection, tag handling, and platform-specific utilities.

## Features

- **Entity ID Detection**: Automatic container ID and entity ID detection from cgroups, Docker, Kubernetes, and Azure App Services
- **Tag Management**: Compile-time validated tags with the `tag!` macro
- **Platform Utilities**: Unix-specific utilities for fork, exec, and process management
- **Datadog Headers**: Standard Datadog HTTP headers and constants
- **Timeout Support**: Configurable timeouts for HTTP requests
- **Worker Abstraction**: Generic worker pattern for background tasks

## Modules

- `azure_app_services`: Azure App Services integration
- `config`: Configuration utilities
- `entity_id`: Container and entity ID detection
- `error`: Common error types
- `header`: Datadog HTTP headers
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

- `std` (default): Enable the standard-library-dependent utilities
- `alloc`: Enable allocation-backed tag creation and parsing without `std`
- `cgroup_testing`: Enable cgroup stubbing for testing

`TagParser` and `TagValidationError` are available without feature flags. To use the allocation-backed
`Tag` from a `no_std` crate:

```bash
cargo check -p libdd-common --lib --no-default-features --features alloc
```
