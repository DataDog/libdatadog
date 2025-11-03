# libdd-trace-protobuf

Protocol buffer definitions for Datadog APM trace data.

## Overview

`libdd-trace-protobuf` provides Rust types and serialization support for Datadog's APM trace protocol buffers.

## Features

- **Trace Protobuf Types**: Complete Rust types for Datadog trace format
- **Prost Integration**: Efficient protobuf serialization using prost
- **Span Serialization**: Serialize spans to Datadog's binary format
- **Type Safety**: Strongly-typed representations of trace data
- **Custom Deserializers**: Specialized deserializers for trace data

## Protocol Buffer Types

The crate provides types for:
- `Span`: Individual trace spans
- `Trace`: Collections of spans
- `TraceChunk`: Chunked trace data
- `ClientStatsPayload`: Statistics payloads
- `APMSample`: APM sampling information

## Example Usage

```rust
use libdd_trace_protobuf::pb;

// Create a span
let span = pb::Span {
    service: "my-service".to_string(),
    name: "http.request".to_string(),
    resource: "GET /api/users".to_string(),
    trace_id: 12345,
    span_id: 67890,
    parent_id: 0,
    start: 1234567890,
    duration: 1000000,
    ..Default::default()
};

// Serialize to protobuf
let bytes = span.encode_to_vec();
```

