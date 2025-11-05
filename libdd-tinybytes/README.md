# libdd-tinybytes

A lightweight alternative to `bytes::Bytes` providing immutable, reference-counted byte buffers with zero-copy cloning and slicing.

## Overview

`libdd-tinybytes` provides an immutable byte buffer type similar to `bytes::Bytes` with a focus on simplicity and `AsRef<[u8]>` support. It uses reference counting to enable efficient zero-copy operations.

## Types

### Bytes

An immutable byte buffer that supports:
- **Zero-copy cloning**: Creating new `Bytes` instances shares the underlying buffer through reference counting
- **Zero-copy slicing**: Extracting subslices without copying the underlying data
- **Static buffers**: Efficient handling of `&'static [u8]` without reference counting overhead
- **Thread safety**: Implements `Send + Sync` for safe use across threads
- **AsRef implementation**: Directly usable as `&[u8]`

### BytesString

A UTF-8 validated string type built on top of `Bytes` (enabled with the `bytes_string` feature):
- **UTF-8 validation**: Ensures data is valid UTF-8 at construction time
- **String interface**: Provides `AsRef<str>` and `Borrow<str>` implementations
- **Zero-copy operations**: Inherits efficient cloning and slicing from `Bytes`

## Implementation Details

The crate uses a custom reference counting implementation optimized for its specific use case, tracking only strong references. Individual deallocations do not free memory; instead, memory is reclaimed when the last reference is dropped.

## Feature Flags

- `bytes_string`: Enable the `BytesString` UTF-8 validated string type
- `serialization`: Enable serde support for serialization

## License

Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/

SPDX-License-Identifier: Apache-2.0
