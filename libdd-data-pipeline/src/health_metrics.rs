// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Trace Exporter Health Metrics
//!
//! This module defines all health metrics emitted by the libdatadog trace exporter. These metrics
//! are sent to DogStatsD to provide visibility for Datadog support.
//!
//! ## Overview
//!
//! Health metrics help monitor the trace exporter's behavior, including successful operations,
//! error conditions, and performance characteristics. They are emitted via DogStatsD and follow
//! consistent naming conventions.
//!
//! **Note**: Health metrics are **disabled by default**. They must be explicitly enabled using
//! `TraceExporterBuilder::enable_health_metrics()` or the FFI function
//! `ddog_trace_exporter_config_enable_health_metrics(config, true)`.
//!
//! ## Metric Types
//!
//! - **Count**: Incremental counters that track the number of occurrences
//! - **Distribution**: Value distributions that track sizes, durations, or quantities
//!
//! ## Naming Convention
//!
//! The metrics follow a hierarchical naming pattern:
//!
//! - `datadog.tracer.exporter.*`: All trace exporter metrics
//!   - `datadog.tracer.exporter.deserialize.*`: Trace deserialization metrics
//!   - `datadog.tracer.exporter.serialize.*`: Trace serialization metrics
//!   - `datadog.tracer.exporter.transport.*`: Network transport metrics
//!     - `datadog.tracer.exporter.transport.traces.*`: Trace-specific transport metrics
//!       - `datadog.tracer.exporter.transport.traces.sent`: All trace send attempts
//!       - `datadog.tracer.exporter.transport.traces.successful`: Successful trace sends
//!       - `datadog.tracer.exporter.transport.traces.failed`: Failed trace sends
//!       - `datadog.tracer.exporter.transport.traces.dropped`: Dropped traces due to errors
//!
//! ## Error Handling Patterns
//!
//! ### HTTP Status Code Handling
//!
//! - **Success (2xx)**: Emit `transport.traces.successful`, `transport.sent.bytes`,
//!   `transport.traces.sent`
//! - **Client Errors (4xx)**: Emit `transport.traces.failed`, `transport.sent.bytes`,
//!   `transport.traces.sent`, and conditionally `transport.dropped.bytes`,
//!   `transport.traces.dropped`
//! - **Server Errors (5xx)**: Emit `transport.traces.failed`, `transport.sent.bytes`,
//!   `transport.traces.sent`, `transport.dropped.bytes`, `transport.traces.dropped`
//! - **Network Errors**: Emit `transport.traces.failed`, `transport.sent.bytes`,
//!   `transport.traces.sent`, `transport.dropped.bytes`, `transport.traces.dropped`
//!
//! ### Special Status Code Exclusions
//!
//! The following HTTP status codes do NOT trigger `transport.dropped.bytes` or
//! `transport.traces.dropped` emission:
//! - **404 Not Found**: Indicates endpoint not available (agent configuration issue)
//! - **415 Unsupported Media Type**: Indicates format negotiation issue
//!
//! These exclusions prevent false alarms for configuration issues rather than actual payload drops.
//!
//! ## Tags
//!
//! All metrics include the following standard tags:
//! - `libdatadog_version`: Version of the libdatadog library
//!
//! Additional conditional tags:
//! - `type:<status_code>`: HTTP status code for error metrics (e.g., `type:400`, `type:404`,
//!   `type:500`)
//! - `type:<error_type>`: Error type classification for non-HTTP errors (e.g., `type:network`,
//!   `type:timeout`, `type:response_body`, `type:build`, `type:unknown`)

// =============================================================================
// Trace Processing Metrics
// =============================================================================

/// Number of trace chunks successfully deserialized from input.
///
/// **Type**: Count  
/// **When Emitted**: After successful deserialization of trace data from msgpack format  
/// **Tags**: `libdatadog_version`
pub(crate) const DESERIALIZE_TRACES: &str = "datadog.tracer.exporter.deserialize.traces";

/// Number of trace deserialization errors.
///
/// **Type**: Count  
/// **When Emitted**: When msgpack deserialization fails due to invalid format or corrupted data  
/// **Tags**: `libdatadog_version`
pub(crate) const DESERIALIZE_TRACES_ERRORS: &str = "datadog.tracer.exporter.deserialize.errors";

/// Number of trace serialization errors.
///
/// **Type**: Count  
/// **When Emitted**: Currently unused but reserved for future trace serialization error tracking  
/// **Tags**: `libdatadog_version`  
/// **Status**: Dead code (marked with `#[allow(dead_code)]`)
#[allow(dead_code)] // TODO (APMSP-1584) Add support for health metrics when using trace utils
pub(crate) const SERIALIZE_TRACES_ERRORS: &str = "datadog.tracer.exporter.serialize.errors";

// =============================================================================
// Transport - Trace Metrics
// =============================================================================

/// Number of trace chunks included in HTTP requests to the agent (all attempts).
///
/// **Type**: Distribution  
/// **When Emitted**: Always emitted for every send attempt, regardless of success or failure  
/// **Tags**: `libdatadog_version`
pub(crate) const TRANSPORT_TRACES_SENT: &str = "datadog.tracer.exporter.transport.traces.sent";

/// Number of trace chunks successfully sent to the agent.
///
/// **Type**: Count  
/// **When Emitted**: After successful HTTP response from the agent (2xx status codes)  
/// **Tags**: `libdatadog_version`
pub(crate) const TRANSPORT_TRACES_SUCCESSFUL: &str =
    "datadog.tracer.exporter.transport.traces.successful";

/// Number of errors encountered while sending traces to the agent.
///
/// **Type**: Count  
/// **When Emitted**:
/// - HTTP error responses (4xx, 5xx status codes)
/// - Network/connection errors
/// - Request timeout errors
///
/// **Tags**: `libdatadog_version`, `type:<status_code>` (for HTTP errors), `type:<error_type>` (for
/// other errors)
///
/// **Error Types**:
/// - `type:<status_code>`: HTTP status codes (e.g., `type:400`, `type:404`, `type:500`)
/// - `type:network`: Network/connection errors
/// - `type:timeout`: Request timeout errors
/// - `type:response_body`: Response body read errors
/// - `type:build`: Request build errors
/// - `type:unknown`: Fallback for unrecognized error types
pub(crate) const TRANSPORT_TRACES_FAILED: &str = "datadog.tracer.exporter.transport.traces.failed";

/// Number of trace chunks dropped due to errors.
///
/// **Type**: Distribution  
/// **When Emitted**:
/// - HTTP error responses (excluding 404 Not Found and 415 Unsupported Media Type)
/// - Network/connection errors
/// - Request timeout errors
///
/// **Tags**: `libdatadog_version`
///
/// **Note**: 404 and 415 status codes are excluded as they represent endpoint/format issues rather
/// than dropped payloads. While they aren't counted as dropped traces, they may still be dropped.
pub(crate) const TRANSPORT_TRACES_DROPPED: &str =
    "datadog.tracer.exporter.transport.traces.dropped";

// =============================================================================
// Transport - Payload Metrics
// =============================================================================

/// Size in bytes of HTTP payloads sent to the agent.
///
/// **Type**: Distribution  
/// **When Emitted**: Always emitted for every send attempt, regardless of success or failure  
/// **Tags**: `libdatadog_version`
pub(crate) const TRANSPORT_SENT_BYTES: &str = "datadog.tracer.exporter.transport.sent.bytes";

/// Size in bytes of HTTP payloads dropped due to errors.
///
/// **Type**: Distribution  
/// **When Emitted**:
/// - HTTP error responses (excluding 404 Not Found and 415 Unsupported Media Type)
/// - Network/connection errors
/// - Request timeout errors
///
/// **Tags**: `libdatadog_version`
///
/// **Note**: 404 and 415 status codes are excluded as they represent endpoint/format issues rather
/// than dropped payloads
pub(crate) const TRANSPORT_DROPPED_BYTES: &str = "datadog.tracer.exporter.transport.dropped.bytes";

/// Number of HTTP requests made to the agent.
///
/// **Type**: Distribution  
/// **When Emitted**: Always emitted after each send operation, counting all HTTP attempts including
/// retries **Tags**: `libdatadog_version`
///
/// **Note**: Value represents total request attempts (1 for success without retries, >1 when
/// retries occur)
pub(crate) const TRANSPORT_REQUESTS: &str = "datadog.tracer.exporter.transport.requests";

#[derive(Debug)]
pub(crate) enum HealthMetric {
    Count(&'static str, i64),
    Distribution(&'static str, i64),
}
