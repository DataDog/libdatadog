# libdd-trace-stats

Compute aggregated statistics from distributed tracing spans with time-bucketed concentration.

## Overview

`libdd-trace-stats` provides utilities for computing trace statistics by aggregating spans into time-based buckets with support for DDSketch distributions.

## Features

- **Span Concentration**: Aggregate spans into time buckets
- **DDSketch Integration**: Use DDSketch for latency distribution metrics
- **Peer Tags**: Support for peer service aggregation
- **Span Filtering**: Filter spans by top-level, measured, or span.kind
- **Time Bucketing**: Configurable bucket sizes for aggregation
- **Statistics Export**: Generate statistics payloads for Datadog backend
- **Stats Exporter** *(non-wasm)*: Periodic HTTP flusher backed by any `HttpClientTrait` implementation

## Span Concentrator

The `SpanConcentrator` is the core component that aggregates spans into statistics:

### Aggregation

Spans are aggregated into time buckets based on their end time. Within each bucket, spans are further aggregated by:
- Service name
- Resource name
- Operation name  
- Span type
- HTTP status code
- Peer tags (if enabled)

### Span Eligibility

Only certain spans are aggregated:
- Root spans
- Top-level spans
- Measured spans
- Spans with eligible `span.kind` values

### Flushing

When flushed, the concentrator keeps the most recent buckets and returns older buckets as statistics.

## Stats Exporter

`StatsExporter` wraps a `FlushableConcentrator` and periodically drains it, encoding the resulting `ClientStatsPayload` as msgpack and POSTing it to the agent's `/v0.6/stats` endpoint.

```rust
use libdd_trace_stats::stats_exporter::{StatsExporter, StatsMetadata};
use libdd_capabilities_impl::NativeCapabilities;
use std::sync::{Arc, Mutex};
use std::time::Duration;

let exporter = StatsExporter::<NativeCapabilities>::new(
    Duration::from_secs(10),
    Arc::new(Mutex::new(concentrator)),
    StatsMetadata { service: "my-service".into(), ..Default::default() },
    endpoint,
    NativeCapabilities::new_client(),
);

// Flush and send (force=true drains all buckets on shutdown)
exporter.send(false).await?;
```

## Example Usage

```rust
use libdd_trace_stats::span_concentrator::SpanConcentrator;
use std::time::{Duration, SystemTime};

// Create a concentrator
let mut concentrator = SpanConcentrator::new(
    Duration::from_secs(10), // 10 second buckets
    SystemTime::now(),
    vec!["client".to_string(), "server".to_string()], // eligible span kinds
    vec!["peer.service".to_string()], // peer tag keys
);

// Add spans
// concentrator.add_span(&span);

// Flush statistics
// let stats = concentrator.flush(false);
```
