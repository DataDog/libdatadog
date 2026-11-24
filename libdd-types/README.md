# libdd-types

Shared foundational types for Datadog libraries.

## Overview

`libdd-types` provides the stable, low-level types used across Datadog Rust
libraries. Its public surface is intentionally small and sealed: attributes are
hidden behind stable constructors, accessors, and builders, so adding fields or
behavior does not break consumers.

## Modules

- `endpoint`: Agent/agentless endpoint URLs with api-key, timeout and resolver settings
- `tag`: Tag creation and validation (compile-time via the `tag!` macro, runtime via `Tag::new`)
- `error`: FFI-safe static error messages (`FfiSafeErrorMessage`)
- `header`: Standard Datadog HTTP header names and content types
- `multipart`: Multipart form-data construction
- `mutable_metadata`: Tracer metadata updatable after startup (runtime ID, process tags, session ancestry)
- lock helpers at the crate root: `MutexExt`, `RwLockExt`, `ResultInfallibleExt`

## Examples

### Creating tags

```rust
use libdd_types::tag;

// Compile-time validated tag
let tag1 = tag!("service", "my-service");

// Runtime tag creation
use libdd_types::tag::Tag;
let tag2 = Tag::new("env", "production")?;
```

## Feature Flags

- `std` (default): Enable the standard-library-dependent utilities
- `alloc`: Enable allocation-backed tag creation and parsing without `std`

`TagParser` and `TagValidationError` are available without feature flags. To use the allocation-backed
`Tag` from a `no_std` crate:

```bash
cargo check -p libdd-types --lib --no-default-features --features alloc
```
