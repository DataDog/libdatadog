# libdd-tinybytes

Space-efficient byte string implementation for Datadog libraries.

## Overview

`libdd-tinybytes` provides memory-efficient byte string types optimized for small strings with inline storage to avoid heap allocations.

## Features

- **Inline Storage**: Small strings stored inline without heap allocation
- **Copy-on-Write**: Efficient cloning with COW semantics
- **Zero-copy**: Support for zero-copy operations
- **Serialization**: Serde support for serialization
- **String Interning**: Optional string interning for deduplication
- **UTF-8 Support**: Optional UTF-8 validation

## Types

- `TinyBytes`: Generic byte string with inline optimization
- `BytesString`: UTF-8 validated byte string

## Example Usage

```rust
use tinybytes::BytesString;

// Small strings stored inline (no allocation)
let small = BytesString::from("hello");

// Large strings heap-allocated
let large = BytesString::from("a very long string that doesn't fit inline...");

// Efficient cloning
let cloned = small.clone(); // No allocation
```

## Benefits

- Reduced allocations for small strings
- Lower memory overhead
- Better cache locality
- Faster cloning

## Feature Flags

- `bytes_string`: Enable BytesString type
- `serialization`: Enable serde support

