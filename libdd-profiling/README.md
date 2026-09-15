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

## C++ bindings

The optional `cxx` feature builds CXX bindings for C++ consumers. Include the convenience header from the generated CXX include directory:

```cpp
#include "datadog/profiling.hpp"
```

The convenience header includes the generated bridge header and provides helper utilities under `datadog::profiling::views`, such as `views::slice(...)` for creating `rust::Slice` values from C++ containers and `views::sample(...)` / `views::dictionary_sample(...)` for constructing sample views.

```cpp
using namespace datadog::profiling;

std::vector<Location> locations{location};
std::vector<std::int64_t> values{1000000};
std::vector<Label> labels{label};

Sample sample = views::sample(locations, values, labels);
```

Factory APIs return result wrappers with `ok()`, `message()`, `check_and_print()`, and `take_value()`. `take_value()` must be called at most once and only after a successful check.

```cpp
auto result = Profile::create({SampleType::WallTime}, period);
if (!result->check_and_print()) return false;
auto profile = result->take_value();
```

Object mutation APIs on `Profile` and `ProfileDictionary` return `bool`; detailed errors are drained from the owning object via `take_errors()`. Profiles and profile dictionaries are quiet by default and store the first error per operation unless configured with `set_error_policy(...)`.

Exporter APIs send serialized profiles and return `Status`, which exposes `ok()`, `operation()`, `message()`, and `check_and_print()`:

```cpp
auto encoded_result = profile->serialize();
if (!encoded_result->check_and_print()) return false;
auto encoded = encoded_result->take_value();
if (!exporter->send_encoded_profile(std::move(encoded), ...).check_and_print()) return false;
```

The existing release builder packages the C ABI artifacts. CXX consumers should use the cargo-built CXX bridge artifacts produced with the `cxx` feature.

## Modules

- `api`: Core API types (ValueType, Period, Mapping, Function, etc.)
- `collections`: String storage and interning for efficient memory use
- `exporter`: HTTP exporter for sending profiles to Datadog
- `internal`: Internal profile management and aggregation
- `iter`: Iteration utilities for profile data
- `pprof`: pprof protobuf format support

## Example Usage

```rust
use libdd_profiling::api::{Profile, SampleType};

// Create a profile
let sample_types = vec![
    SampleType::CpuSamples,
    SampleType::CpuTime,
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

