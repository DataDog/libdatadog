# libdd-trace-utils

Utilities for handling distributed tracing spans, serialization, and data transport to Datadog APM.

## Overview

`libdd-trace-utils` provides essential utilities for distributed tracing including span processing, MessagePack encoding/decoding, payload handling, and HTTP transport with retry logic.

## Features

- **Span Processing**: Utilities for processing and manipulating trace spans
- **MessagePack Support**: Encoding and decoding of traces in MessagePack format (v04 and v05)
- **HTTP Transport**: Send trace data to Datadog with retry logic
- **Payload Management**: Efficient payload construction and size management
- **Statistics Utilities**: Trace statistics computation and aggregation
- **Configuration Helpers**: Utilities for tracer configuration
- **Test Utilities**: Mock Datadog test agent for integration testing
- **Header Tags**: Tracer header tag handling

## Modules

- `trace_utils`: Core trace processing utilities
- `msgpack_encoder`: MessagePack encoding for spans (v04/v05 formats)
- `msgpack_decoder`: MessagePack decoding for spans (v04/v05 formats)
- `send_data`: HTTP transport layer for sending traces
- `send_with_retry`: Retry logic for failed requests
- `stats_utils`: Statistics computation utilities
- `config_utils`: Configuration helpers
- `tracer_header_tags`: Header tag management
- `tracer_payload`: Payload construction and management
- `span`: Span types and utilities
- `test_utils`: Testing utilities (feature gated)

## Feature Flags

- `https` (default): Enable HTTPS support
- `mini_agent`: Enable mini-agent features (proxy + compression)
- `proxy`: HTTP proxy support
- `compression`: Zstd and flate2 compression
- `test-utils`: Enable test utilities and mock agent
- `fips`: Use FIPS-compliant cryptography

## Example Usage

```rust
use libdd_trace_utils::trace_utils::SendData;
use libdd_trace_utils::tracer_payload::TracerPayloadCollection;

// Send traces to Datadog
let payload = TracerPayloadCollection::new(/* ... */);
// ... configure and send ...
```

## Testing

The crate includes a mock Datadog test agent for integration testing:

```rust
#[cfg(feature = "test-utils")]
use libdd_trace_utils::test_utils::datadog_test_agent;

// Use mock agent in tests
```

