# libdd-common

Common utilities and foundational components shared across Datadog telemetry libraries.

## Overview

`libdd-common` provides shared functionality used across multiple Datadog Rust libraries including endpoint handling and tag handling.

## Features

- **Tag Management**: Compile-time validated tags with the `tag!` macro
- **Datadog Headers**: Standard Datadog HTTP headers and constants

## Modules

- `error`: Common error types
- `header`: Datadog HTTP headers
- `multipart`: Multipart form data construction
- `mutable_metadata`: Tracer metadata updatable after startup (runtime ID, process tags, session ancestry)
- `tag`: Tag creation and validation

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

## Features Flags

- `std` (default): Enable the standard-library-dependent utilities
- `alloc`: Enable allocation-backed tag creation and parsing without `std`

`TagParser` and `TagValidationError` are available without feature flags. To use the allocation-backed
`Tag` from a `no_std` crate:

```bash
cargo check -p libdd-common --lib --no-default-features --features alloc
```
