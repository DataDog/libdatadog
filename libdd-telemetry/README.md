# libdd-telemetry

Internal telemetry library for reporting Datadog library metrics and events.

## Overview

`libdd-telemetry` provides telemetry collection and reporting for Datadog libraries, allowing them to report their own operational metrics, configuration, and errors back to Datadog.

## Features

- **Metrics Collection**: Collect library performance metrics
- **Configuration Reporting**: Report library configuration
- **Error Tracking**: Track and report library errors
- **Host Information**: Automatic host and container metadata
- **HTTP Transport**: Send telemetry data to Datadog intake
- **Constrained Submission**: Submit preallocated metrics without `std` or an allocator
- **Worker Pattern**: Async background telemetry worker
- **Dependency Tracking**: Report library dependencies
- **Application Metadata**: Track application information

## Modules

- `config`: Telemetry configuration
- `data`: Telemetry data types and structures
- `info`: System and host information gathering
- `metrics`: Metrics collection and aggregation
- `signal_safe`: Fixed-buffer metric encoding and submission
- `worker`: Background telemetry worker

## Telemetry Data Types

The library collects and reports:
- **Metrics**: Count, gauge, rate, and distribution metrics
- **Logs**: Library log events
- **Configurations**: Library configuration changes
- **Dependencies**: Loaded library versions
- **Integrations**: Active integrations
- **App Started**: Application lifecycle events

## Example Usage

```rust
use libdd_telemetry::{build_host, data};

// Build host information
let host = build_host();

// Create telemetry data
let app = data::Application {
    service_name: "my-service".to_string(),
    env: Some("production".to_string()),
    // ...
};
```

## Signal-safe metric submission

The `signal-safe` feature builds without `std` and uses the blocking `rustix`
TCP transport from `libdd-http-client-lite` with caller-owned resolvers and
request/response buffers. It is intended for code paths where allocation,
runtime startup, and process-global configuration are not available. Whether a
call is async-signal-safe still depends on the platform implementations
supplied by the caller.

The no-std library build can be checked directly:

```bash
cargo +nightly-2026-02-08 build \
  -p libdd-telemetry \
  --lib \
  --no-default-features \
  --features signal-safe \
  --target x86_64-unknown-linux-none \
  -Zbuild-std=core,compiler_builtins \
  -Zbuild-std-features=compiler-builtins-mem
```

The example posts a real `generate-metrics` payload to the Agent at
`127.0.0.1:8126` while keeping telemetry serialization and HTTP buffers
caller-owned:

```bash
cargo run -p libdd-telemetry \
  --example signal_safe_metrics \
  --no-default-features \
  --features signal-safe
```

## Host Information

Automatically gathers:
- Hostname
- Container ID
- OS name and version
- Kernel information
- Entity ID
