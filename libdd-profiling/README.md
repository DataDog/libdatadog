# libdd-profiling

Core profiling library for collecting, aggregating, and exporting profiling data in pprof format to Datadog.

## Overview

`libdd-profiling` provides the core functionality for continuous profiling, including profile collection, aggregation, compression, and export to Datadog backends using the pprof format.

## Features

- **Profile Management**: Collect and manage profiling data (CPU, memory, allocations, etc.)
- **Sample Aggregation**: Efficiently aggregate samples with stack traces
- **pprof Format**: Generate profiles in Google's pprof protobuf format
- **Compression**: LZ4 compression for efficient data transfer
- **Stack Traces**: Full stack trace capture with mapping and function information
- **Value Types**: Support for multiple value types (CPU time, memory, count, etc.)
- **Upscaling**: Statistical upscaling for sampled data
- **HTTP Export**: Built-in HTTP exporter with multipart form data support

## Modules

- `api`: Core API types (ValueType, Period, Mapping, Function, etc.)
- `collections`: String storage and interning for efficient memory use
- `exporter`: HTTP exporter for sending profiles to Datadog
- `internal`: Internal profile management and aggregation
- `iter`: Iteration utilities for profile data
- `pprof`: pprof protobuf format support

## Example Usage

```rust
use libdd_profiling::api::{Profile, ValueType};

// Create a profile
let value_types = vec![
    ValueType::new("samples", "count"),
    ValueType::new("cpu", "nanoseconds"),
];

// Add samples with stack traces
// ... collect profiling data ...

// Export to Datadog
// ... use exporter to send profile ...
```

## Profile Format

The library generates profiles in the pprof format, which includes:
- Stack traces with function names and locations
- Sample values (counts, times, sizes)
- Mappings for binaries and shared libraries
- Labels for additional context

