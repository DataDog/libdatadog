# Trace Exporter Health Metrics

This document defines all health metrics emitted by the libdatadog trace exporter. These metrics are sent to DogStatsD
to provide visibility into the health and performance of the trace export process.

## Overview

Health metrics help monitor the trace exporter's behavior, including successful operations, error conditions, and
performance characteristics. They are emitted via DogStatsD and follow consistent naming conventions.

## Metric Types

- **Count**: Incremental counters that track the number of occurrences
- **Distribution**: Value distributions that track sizes, durations, or quantities

## Metrics Reference

### Trace Processing Metrics

#### `datadog.libdatadog.deser_traces`
- **Type**: Count
- **Description**: Number of trace chunks successfully deserialized from input
- **When Emitted**: After successful deserialization of trace data from msgpack format
- **Tags**: `libdatadog_version`

#### `datadog.libdatadog.deser_traces.errors`
- **Type**: Count  
- **Description**: Number of trace deserialization errors
- **When Emitted**: When msgpack deserialization fails due to invalid format or corrupted data
- **Tags**: `libdatadog_version`

#### `datadog.libdatadog.send.traces`
- **Type**: Count
- **Description**: Number of trace chunks successfully sent to the agent
- **When Emitted**: After successful HTTP response from the agent (2xx status codes)
- **Tags**: `libdatadog_version`

#### `datadog.libdatadog.send.traces.errors`
- **Type**: Count
- **Description**: Number of errors encountered while sending traces to the agent
- **When Emitted**: 
  - HTTP error responses (4xx, 5xx status codes)
  - Network/connection errors
  - Request timeout errors
- **Tags**: `libdatadog_version`, `type:<status_code>` (for HTTP errors), `type:<error_type>` (for other errors)
- **Error Types**: 
  - `type:<status_code>`: HTTP status codes (e.g., `type:400`, `type:404`, `type:500`)
  - `type:network`: Network/connection errors
  - `type:timeout`: Request timeout errors
  - `type:response_body`: Response body read errors
  - `type:build`: Request build errors
  - `type:unknown`: Fallback for unrecognized error types

### HTTP Transport Metrics

#### `datadog.tracer.http.sent.bytes`
- **Type**: Distribution
- **Description**: Size in bytes of HTTP payloads sent to the agent
- **When Emitted**: Always emitted for every send attempt, regardless of success or failure
- **Tags**: `libdatadog_version`

#### `datadog.tracer.http.sent.traces`
- **Type**: Distribution
- **Description**: Number of trace chunks included in HTTP requests to the agent
- **When Emitted**: Always emitted for every send attempt, regardless of success or failure
- **Tags**: `libdatadog_version`

#### `datadog.tracer.http.dropped.bytes`
- **Type**: Distribution
- **Description**: Size in bytes of HTTP payloads dropped due to errors
- **When Emitted**: 
  - HTTP error responses (excluding 404 Not Found and 415 Unsupported Media Type)
  - Network/connection errors
  - Request timeout errors
- **Tags**: `libdatadog_version`
- **Note**: 404 and 415 status codes are excluded as they represent endpoint/format issues rather than dropped payloads

#### `datadog.tracer.http.dropped.traces`
- **Type**: Distribution
- **Description**: Number of trace chunks dropped due to errors
- **When Emitted**: 
  - HTTP error responses (excluding 404 Not Found and 415 Unsupported Media Type)
  - Network/connection errors
  - Request timeout errors
- **Tags**: `libdatadog_version`
- **Note**: 404 and 415 status codes are excluded as they represent endpoint/format issues rather than dropped payloads

#### `datadog.tracer.http.requests`
- **Type**: Distribution
- **Description**: Number of HTTP requests made to the agent
- **When Emitted**: Always emitted after each send operation, counting all HTTP attempts including retries
- **Tags**: `libdatadog_version`
- **Note**: Value represents total request attempts (1 for success without retries, >1 when retries occur)

### Serialization Metrics

#### `datadog.libdatadog.ser_traces.errors`
- **Type**: Count
- **Description**: Number of trace serialization errors
- **When Emitted**: Currently unused but reserved for future trace serialization error tracking
- **Tags**: `libdatadog_version`
- **Status**: Dead code (marked with `#[allow(dead_code)]`)

## Naming Convention

The metrics follow a hierarchical naming pattern:

- `datadog.libdatadog.*`: Internal libdatadog operation metrics
- `datadog.tracer.*`: Tracer-level metrics for HTTP transport and request handling

## Error Handling Patterns

### HTTP Status Code Handling

- **Success (2xx)**: Emit `send.traces`, `http.sent.bytes`, `http.sent.traces`
- **Client Errors (4xx)**: Emit `send.traces.errors`, `http.sent.bytes`, `http.sent.traces`, and conditionally 
  `http.dropped.bytes`, `http.dropped.traces`
- **Server Errors (5xx)**: Emit `send.traces.errors`, `http.sent.bytes`, `http.sent.traces`, `http.dropped.bytes`, `http.dropped.traces`
- **Network Errors**: Emit `send.traces.errors`, `http.sent.bytes`, `http.sent.traces`, `http.dropped.bytes`, `http.dropped.traces`

### Special Status Code Exclusions

The following HTTP status codes do NOT trigger `http.dropped.bytes` or `http.dropped.traces` emission:
- **404 Not Found**: Indicates endpoint not available (agent configuration issue)
- **415 Unsupported Media Type**: Indicates format negotiation issue

These exclusions prevent false alarms for configuration issues rather than actual payload drops.

## Tags

All metrics include the following standard tags:
- `libdatadog_version`: Version of the libdatadog library

Additional conditional tags:
- `type:<status_code>`: HTTP status code for error metrics (e.g., `type:400`, `type:404`, `type:500`)
- `type:<error_type>`: Error type classification for non-HTTP errors (e.g., `type:network`, `type:timeout`, `type:response_body`, `type:build`, `type:unknown`)