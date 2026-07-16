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
- **Alloc-only Data Model**: Build and serialize telemetry payloads without `std`
- **Worker Pattern**: Async background telemetry worker
- **Dependency Tracking**: Report library dependencies
- **Application Metadata**: Track application information

## Modules

- `config`: Telemetry configuration
- `data`: Telemetry data types and structures
- `info`: System and host information gathering
- `metrics`: Metrics collection and aggregation
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

## Alloc-only data model

The `alloc` feature exposes the complete `data` module, including telemetry
envelopes, payloads, metrics, and allocation-backed tags, in a `no_std` build.
Runtime services such as configuration, host discovery, aggregation, and the
worker remain behind the default `std` feature.

```bash
cargo check -p libdd-telemetry --lib --no-default-features --features alloc
```

The metric `Tag` is defined by the alloc-compatible `libdd-common` crate, so
existing `libdd_common::tag::Tag` callers continue to use the same concrete
type.

## Host Information

Automatically gathers:
- Hostname
- Container ID
- OS name and version
- Kernel information
- Entity ID
