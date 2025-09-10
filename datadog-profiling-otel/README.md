# datadog-profiling-otel

This module provides Rust bindings for the OpenTelemetry profiling protobuf definitions, generated using the `prost` library.
This crate implements serialization of data into the otel profiling format; if you're building a profiler you usually don't want to use this crate directly, and instead should use datadog-profiling and ask it to serialize using ottel.

## Usage

See the [basic_usage.rs](examples/basic_usage.rs) example for a complete demonstration of how to create OpenTelemetry profile data.

### Running Examples

```bash
# Run the basic usage example
cargo run --example basic_usage

# Run tests
cargo test

# Build
cargo build
```


