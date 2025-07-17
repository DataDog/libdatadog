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
- **Tags**: `libdatadog_version`, `response_code` (for HTTP errors)

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
  `http.dropped.bytes`
- **Server Errors (5xx)**: Emit `send.traces.errors`, `http.sent.bytes`, `http.sent.traces`, `http.dropped.bytes`
- **Network Errors**: Emit `send.traces.errors`, `http.sent.bytes`, `http.sent.traces`, `http.dropped.bytes`

### Special Status Code Exclusions

The following HTTP status codes do NOT trigger `http.dropped.bytes` emission:
- **404 Not Found**: Indicates endpoint not available (agent configuration issue)
- **415 Unsupported Media Type**: Indicates format negotiation issue

These exclusions prevent false alarms for configuration issues rather than actual payload drops.

## Tags

All metrics include the following standard tags:
- `libdatadog_version`: Version of the libdatadog library

Additional conditional tags:
- `response_code`: HTTP status code (only for HTTP error metrics)