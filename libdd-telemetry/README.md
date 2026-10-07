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
- **Constrained Submission**: Encode borrowed metrics into caller-provided buffers
- **Worker Pattern**: Async background telemetry worker
- **Dependency Tracking**: Report library dependencies
- **Application Metadata**: Track application information

## Modules

- `config`: Telemetry configuration
- `data`: Telemetry data types and structures
- `info`: System and host information gathering
- `metrics`: Metrics collection and aggregation
- `signal_safe`: Allocation-free borrowed metrics and constrained submission
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

The `alloc` feature exposes the whole `data` module (request envelopes and
payloads, application and host metadata, metric series, and allocation-backed
tags) in a `no_std` build. Configuration, host discovery, aggregation, and the
worker remain behind the default `std` feature.

```bash
cargo check -p libdd-telemetry --lib --no-default-features --features alloc
```

With no features at all, the metric namespace and type enums
(`data::metrics::{MetricNamespace, MetricType}`) remain available without
`alloc`.

The metric `Tag` is defined by the alloc-compatible `libdd-common` crate, so
existing `libdd_common::tag::Tag` callers continue to use the same concrete
type.

## Allocation-free metric submission

The `signal_safe` module borrows preallocated [`Application`](src/data/common.rs)
and [`Host`](src/data/common.rs) values together with metric-series, point, and
tag data. It serializes `generate-metrics` requests directly into a
caller-provided slice with `serde-json-core`. Metric and tag collections are
borrowed as slices, so arrays and fixed-capacity collections such as
`heapless::Vec` can be passed without making `libdd-telemetry` depend on their
container type.

The model and `encode_metrics` are available with the `alloc` feature. The
`signal-safe` feature implies `alloc` and additionally enables `send_metrics`
using vanilla `reqwless` and a caller-provided transport:

```bash
cargo check -p libdd-telemetry --lib --no-default-features --features alloc
cargo check -p libdd-telemetry --lib --no-default-features --features signal-safe
cargo run -p libdd-telemetry --example signal_safe_metrics --no-default-features --features signal-safe
```

Allocate and populate the application and host identity before entering the
signal-safe section, then keep them alive and immutable while requests borrow
them. The encoding and HTTP layers do not allocate. Async-signal-safety of a
full submission still depends on the transport, resolver, executor, and
buffers provided by the caller.

`send_metrics` accepts up to `MAX_EXTRA_HEADERS` caller-provided headers, such
as `DD-API-KEY` for direct intake submission. Each request is sent with
`Connection: close` and its response body is not read, so use a fresh
connection per call.

## Host Information

Automatically gathers:
- Hostname
- Container ID
- OS name and version
- Kernel information
- Entity ID
