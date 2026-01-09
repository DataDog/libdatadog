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

use std::borrow::Cow;

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
/// **When Emitted**: When msgpack serialization fails  
/// **Tags**: `libdatadog_version`
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
#[cfg_attr(test, derive(PartialEq))]
pub(crate) enum HealthMetric {
    Count(&'static str, i64),
    Distribution(&'static str, i64),
}

/// Categorization of errors from different sources (direct hyper responses vs
/// send_with_retry results) for consistent metric emission
#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub(crate) enum TransportErrorType {
    /// HTTP error with a specific status code (4xx, 5xx)
    Http(u16),
    /// Network/connection error
    Network,
    /// Request timeout
    Timeout,
    /// Failed to read response body
    ResponseBody,
    /// Failed to build the HTTP request
    Build,
}

impl TransportErrorType {
    pub(crate) fn as_tag_value(&self) -> Cow<'static, str> {
        match self {
            TransportErrorType::Http(code) => Cow::Owned(code.to_string()),
            TransportErrorType::Network => Cow::Borrowed("network"),
            TransportErrorType::Timeout => Cow::Borrowed("timeout"),
            TransportErrorType::ResponseBody => Cow::Borrowed("response_body"),
            TransportErrorType::Build => Cow::Borrowed("build"),
        }
    }

    /// Per the health metrics specification:
    /// - 404 and 415 status codes do NOT emit dropped metrics
    /// - All other HTTP errors and non-HTTP errors emit dropped metrics
    pub(crate) fn should_emit_dropped_metrics(&self) -> bool {
        !matches!(
            self,
            TransportErrorType::Http(404) | TransportErrorType::Http(415)
        )
    }
}

/// Result structure for health metrics emission
///
/// This structure captures all the information needed to emit the appropriate
/// health metric for a send operation regardless whence it came
#[derive(Debug)]
#[cfg_attr(test, derive(Clone, PartialEq))]
pub(crate) struct SendResult {
    /// The error type if the operation failed, or `None` if it succeeded.
    pub error_type: Option<TransportErrorType>,
    /// Size of the payload in bytes
    pub payload_bytes: usize,
    /// Number of trace chunks in the payload
    pub trace_chunks: usize,
    /// Number of HTTP request attempts (including retries)
    pub request_attempts: u32,
}

impl SendResult {
    /// Create a new successful send result
    pub(crate) fn success(
        payload_bytes: usize,
        trace_chunks: usize,
        request_attempts: u32,
    ) -> Self {
        debug_assert!(
            request_attempts > 0,
            "SendResult::success called with zero request attempts"
        );
        Self {
            error_type: None,
            payload_bytes,
            trace_chunks,
            request_attempts,
        }
    }

    /// Create a new failed send result
    pub(crate) fn failure(
        error_type: TransportErrorType,
        payload_bytes: usize,
        trace_chunks: usize,
        request_attempts: u32,
    ) -> Self {
        debug_assert!(
            request_attempts > 0,
            "SendResult::failure called with zero request attempts"
        );
        Self {
            error_type: Some(error_type),
            payload_bytes,
            trace_chunks,
            request_attempts,
        }
    }

    /// Returns whether the send operation was successful
    #[cfg(test)]
    pub(crate) fn is_success(&self) -> bool {
        self.error_type.is_none()
    }

    /// Collect all health metrics that should be emitted for this result
    ///
    /// This method encapsulates all the logic for determining which metrics to
    /// emit based on the send operation. It returns a vector of metrics that
    /// should be sent to DogStatsD
    ///
    /// # Returns
    ///
    /// A vector of `(HealthMetric, Option<String>)` tuples where:
    /// - The first element is the metric to emit
    /// - The second element is an optional tag value for error classification
    pub(crate) fn collect_metrics(&self) -> Vec<(HealthMetric, Option<String>)> {
        // Max capacity: 3 base + 1 outcome + 2 dropped
        let mut metrics = Vec::with_capacity(6);

        // Always emit: sent bytes, sent traces, request count
        metrics.push((
            HealthMetric::Distribution(TRANSPORT_SENT_BYTES, self.payload_bytes as i64),
            None,
        ));
        metrics.push((
            HealthMetric::Distribution(TRANSPORT_TRACES_SENT, self.trace_chunks as i64),
            None,
        ));
        metrics.push((
            HealthMetric::Distribution(TRANSPORT_REQUESTS, self.request_attempts as i64),
            None,
        ));

        match &self.error_type {
            None => {
                // Emit successful traces count
                metrics.push((
                    HealthMetric::Count(TRANSPORT_TRACES_SUCCESSFUL, self.trace_chunks as i64),
                    None,
                ));
            }
            Some(error_type) => {
                // Emit failed metric with type tag
                metrics.push((
                    HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
                    Some(error_type.as_tag_value().into_owned()),
                ));

                if error_type.should_emit_dropped_metrics() {
                    metrics.push((
                        HealthMetric::Distribution(
                            TRANSPORT_DROPPED_BYTES,
                            self.payload_bytes as i64,
                        ),
                        None,
                    ));
                    metrics.push((
                        HealthMetric::Distribution(
                            TRANSPORT_TRACES_DROPPED,
                            self.trace_chunks as i64,
                        ),
                        None,
                    ));
                }
            }
        }

        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only extension methods for SendResult
    impl SendResult {
        /// Create a `SendResult` from a `SendWithRetryResult`.
        ///
        /// This conversion handles all variants of the retry result and extracts the
        /// appropriate error type and attempt count.
        ///
        /// # Arguments
        ///
        /// * `result` - The result from `send_with_retry`
        /// * `payload_bytes` - Size of the payload that was sent
        /// * `trace_chunks` - Number of trace chunks in the payload
        pub(crate) fn from_retry_result(
            result: &libdd_trace_utils::send_with_retry::SendWithRetryResult,
            payload_bytes: usize,
            trace_chunks: usize,
        ) -> Self {
            use libdd_trace_utils::send_with_retry::SendWithRetryError;

            match result {
                Ok((response, attempts)) => {
                    if response.status().is_success() {
                        Self::success(payload_bytes, trace_chunks, *attempts)
                    } else {
                        // Non-success status in Ok variant (shouldn't happen with
                        // send_with_retry)
                        Self::failure(
                            TransportErrorType::Http(response.status().as_u16()),
                            payload_bytes,
                            trace_chunks,
                            *attempts,
                        )
                    }
                }
                Err(err) => {
                    let (error_type, attempts) = match err {
                        SendWithRetryError::Http(response, attempts) => (
                            TransportErrorType::Http(response.status().as_u16()),
                            *attempts,
                        ),
                        SendWithRetryError::Timeout(attempts) => {
                            (TransportErrorType::Timeout, *attempts)
                        }
                        SendWithRetryError::Network(_, attempts) => {
                            (TransportErrorType::Network, *attempts)
                        }
                        SendWithRetryError::Build(attempts) => {
                            (TransportErrorType::Build, *attempts)
                        }
                    };
                    Self::failure(error_type, payload_bytes, trace_chunks, attempts)
                }
            }
        }
    }

    #[test]
    fn test_http_tag_value() {
        assert_eq!(TransportErrorType::Http(400).as_tag_value().as_ref(), "400");
        assert_eq!(TransportErrorType::Http(404).as_tag_value().as_ref(), "404");
        assert_eq!(TransportErrorType::Http(500).as_tag_value().as_ref(), "500");
    }

    #[test]
    fn test_non_http_tag_values() {
        assert_eq!(
            TransportErrorType::Network.as_tag_value().as_ref(),
            "network"
        );
        assert_eq!(
            TransportErrorType::Timeout.as_tag_value().as_ref(),
            "timeout"
        );
        assert_eq!(
            TransportErrorType::ResponseBody.as_tag_value().as_ref(),
            "response_body"
        );
        assert_eq!(TransportErrorType::Build.as_tag_value().as_ref(), "build");
    }

    #[test]
    fn test_dropped_excludes_404_and_415() {
        assert!(!TransportErrorType::Http(404).should_emit_dropped_metrics());
        assert!(!TransportErrorType::Http(415).should_emit_dropped_metrics());
    }

    #[test]
    fn test_dropped_includes_other_http() {
        assert!(TransportErrorType::Http(400).should_emit_dropped_metrics());
        assert!(TransportErrorType::Http(401).should_emit_dropped_metrics());
        assert!(TransportErrorType::Http(403).should_emit_dropped_metrics());
        assert!(TransportErrorType::Http(500).should_emit_dropped_metrics());
        assert!(TransportErrorType::Http(502).should_emit_dropped_metrics());
        assert!(TransportErrorType::Http(503).should_emit_dropped_metrics());
    }

    #[test]
    fn test_dropped_includes_non_http() {
        assert!(TransportErrorType::Network.should_emit_dropped_metrics());
        assert!(TransportErrorType::Timeout.should_emit_dropped_metrics());
        assert!(TransportErrorType::ResponseBody.should_emit_dropped_metrics());
        assert!(TransportErrorType::Build.should_emit_dropped_metrics());
    }

    #[test]
    fn test_success_construction() {
        let result = SendResult::success(1024, 5, 1);

        assert!(result.is_success());
        assert_eq!(result.error_type, None);
        assert_eq!(result.payload_bytes, 1024);
        assert_eq!(result.trace_chunks, 5);
        assert_eq!(result.request_attempts, 1);
    }

    #[test]
    fn test_failure_construction() {
        let result = SendResult::failure(TransportErrorType::Http(500), 2048, 10, 3);

        assert!(!result.is_success());
        assert_eq!(result.error_type, Some(TransportErrorType::Http(500)));
        assert_eq!(result.payload_bytes, 2048);
        assert_eq!(result.trace_chunks, 10);
        assert_eq!(result.request_attempts, 3);
    }

    #[test]
    fn test_success_metrics() {
        let result = SendResult::success(1024, 5, 1);
        let metrics = result.collect_metrics();

        // Should emit 4 metrics for success
        assert_eq!(metrics.len(), 4);
        assert!(metrics.contains(&(HealthMetric::Distribution(TRANSPORT_SENT_BYTES, 1024), None)));
        assert!(metrics.contains(&(HealthMetric::Distribution(TRANSPORT_TRACES_SENT, 5), None)));
        assert!(metrics.contains(&(HealthMetric::Distribution(TRANSPORT_REQUESTS, 1), None)));
        assert!(metrics.contains(&(HealthMetric::Count(TRANSPORT_TRACES_SUCCESSFUL, 5), None)));
    }

    #[test]
    fn test_success_no_failure_metrics() {
        let result = SendResult::success(1024, 5, 1);
        let metrics = result.collect_metrics();

        for (metric, _) in &metrics {
            match metric {
                HealthMetric::Count(name, _) => {
                    assert_ne!(*name, TRANSPORT_TRACES_FAILED);
                }
                HealthMetric::Distribution(name, _) => {
                    assert_ne!(*name, TRANSPORT_DROPPED_BYTES);
                    assert_ne!(*name, TRANSPORT_TRACES_DROPPED);
                }
            }
        }
    }

    #[test]
    fn test_http_400_emits_dropped() {
        let result = SendResult::failure(TransportErrorType::Http(400), 2048, 10, 5);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 6);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("400".to_string())
        )));
        assert!(metrics.contains(&(
            HealthMetric::Distribution(TRANSPORT_DROPPED_BYTES, 2048),
            None
        )));
        assert!(metrics.contains(&(
            HealthMetric::Distribution(TRANSPORT_TRACES_DROPPED, 10),
            None
        )));
    }

    #[test]
    fn test_http_404_skips_dropped() {
        let result = SendResult::failure(TransportErrorType::Http(404), 2048, 10, 5);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 4);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("404".to_string())
        )));
        for (metric, _) in &metrics {
            if let HealthMetric::Distribution(name, _) = metric {
                assert_ne!(*name, TRANSPORT_DROPPED_BYTES);
                assert_ne!(*name, TRANSPORT_TRACES_DROPPED);
            }
        }
    }

    #[test]
    fn test_http_415_skips_dropped() {
        let result = SendResult::failure(TransportErrorType::Http(415), 1024, 3, 1);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 4);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("415".to_string())
        )));
    }

    #[test]
    fn test_network_error_emits_dropped() {
        let result = SendResult::failure(TransportErrorType::Network, 512, 2, 3);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 6);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("network".to_string())
        )));
        assert!(metrics.contains(&(
            HealthMetric::Distribution(TRANSPORT_DROPPED_BYTES, 512),
            None
        )));
    }

    #[test]
    fn test_timeout_emits_dropped() {
        let result = SendResult::failure(TransportErrorType::Timeout, 1024, 5, 5);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 6);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("timeout".to_string())
        )));
        assert!(metrics.contains(&(
            HealthMetric::Distribution(TRANSPORT_DROPPED_BYTES, 1024),
            None
        )));
    }

    #[test]
    fn test_build_error_emits_dropped() {
        let result = SendResult::failure(TransportErrorType::Build, 256, 1, 1);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 6);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("build".to_string())
        )));
        assert!(metrics.contains(&(
            HealthMetric::Distribution(TRANSPORT_DROPPED_BYTES, 256),
            None
        )));
    }

    #[test]
    fn test_response_body_error_emits_dropped() {
        let result = SendResult::failure(TransportErrorType::ResponseBody, 4096, 20, 1);
        let metrics = result.collect_metrics();

        assert_eq!(metrics.len(), 6);
        assert!(metrics.contains(&(
            HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
            Some("response_body".to_string())
        )));
    }

    #[test]
    fn test_base_metrics_always_emitted() {
        let scenarios = vec![
            SendResult::success(100, 1, 1),
            SendResult::failure(TransportErrorType::Http(500), 200, 2, 2),
            SendResult::failure(TransportErrorType::Network, 300, 3, 3),
            SendResult::failure(TransportErrorType::Http(404), 400, 4, 4),
        ];

        for result in scenarios {
            let metrics = result.collect_metrics();

            let has_sent_bytes = metrics.iter().any(|(m, _)| {
                matches!(m, HealthMetric::Distribution(name, _) if *name == TRANSPORT_SENT_BYTES)
            });
            assert!(has_sent_bytes, "Missing sent_bytes for {:?}", result);

            let has_sent_traces = metrics.iter().any(|(m, _)| {
                matches!(m, HealthMetric::Distribution(name, _) if *name == TRANSPORT_TRACES_SENT)
            });
            assert!(has_sent_traces, "Missing sent_traces for {:?}", result);

            let has_requests = metrics.iter().any(|(m, _)| {
                matches!(m, HealthMetric::Distribution(name, _) if *name == TRANSPORT_REQUESTS)
            });
            assert!(has_requests, "Missing requests for {:?}", result);
        }
    }

    #[test]
    fn test_request_attempts_reflects_retries() {
        let result = SendResult::failure(TransportErrorType::Http(503), 1024, 5, 5);
        let metrics = result.collect_metrics();

        assert!(metrics.contains(&(HealthMetric::Distribution(TRANSPORT_REQUESTS, 5), None)));
    }

    mod send_with_retry_conversion {
        use super::*;
        use bytes::Bytes;
        use hyper::{Response, StatusCode};
        use libdd_common::hyper_migration;
        use libdd_trace_utils::send_with_retry::{SendWithRetryError, SendWithRetryResult};

        /// Helper to create a mock HTTP response for testing
        fn mock_response(status: StatusCode) -> hyper_migration::HttpResponse {
            hyper_migration::mock_response(
                Response::builder().status(status),
                Bytes::from("test body"),
            )
            .unwrap()
        }

        #[test]
        fn test_from_retry_result_success_2xx() {
            let response = mock_response(StatusCode::OK);
            let retry_result: SendWithRetryResult = Ok((response, 1));

            let send_result = SendResult::from_retry_result(&retry_result, 1024, 5);

            assert!(send_result.is_success());
            assert_eq!(send_result.payload_bytes, 1024);
            assert_eq!(send_result.trace_chunks, 5);
            assert_eq!(send_result.request_attempts, 1);
        }

        #[test]
        fn test_from_retry_result_http_error() {
            let response = mock_response(StatusCode::BAD_REQUEST);
            let retry_result: SendWithRetryResult = Err(SendWithRetryError::Http(response, 3));

            let send_result = SendResult::from_retry_result(&retry_result, 2048, 10);

            assert_eq!(send_result.error_type, Some(TransportErrorType::Http(400)));
            assert_eq!(send_result.request_attempts, 3);
        }

        #[test]
        fn test_from_retry_result_timeout_error() {
            let retry_result: SendWithRetryResult = Err(SendWithRetryError::Timeout(5));

            let send_result = SendResult::from_retry_result(&retry_result, 512, 2);

            assert_eq!(send_result.error_type, Some(TransportErrorType::Timeout));
            assert_eq!(send_result.request_attempts, 5);
        }

        #[test]
        fn test_from_retry_result_network_error() {
            // We can't really simulate network error, so we test the behavior
            // via the API directly
            let send_result = SendResult::failure(TransportErrorType::Network, 256, 1, 3);

            assert_eq!(send_result.error_type, Some(TransportErrorType::Network));
            assert_eq!(send_result.request_attempts, 3);

            let metrics = send_result.collect_metrics();
            assert!(metrics.contains(&(
                HealthMetric::Count(TRANSPORT_TRACES_FAILED, 1),
                Some("network".to_string())
            )));
            assert!(metrics.contains(&(
                HealthMetric::Distribution(TRANSPORT_DROPPED_BYTES, 256),
                None
            )));
        }

        #[test]
        fn test_from_retry_result_build_error() {
            let retry_result: SendWithRetryResult = Err(SendWithRetryError::Build(1));

            let send_result = SendResult::from_retry_result(&retry_result, 100, 1);

            assert_eq!(send_result.error_type, Some(TransportErrorType::Build));
            assert_eq!(send_result.request_attempts, 1);
        }

        #[test]
        fn test_from_retry_result_preserves_context() {
            let response = mock_response(StatusCode::OK);
            let retry_result: SendWithRetryResult = Ok((response, 2));

            let send_result = SendResult::from_retry_result(&retry_result, 4096, 25);

            assert_eq!(send_result.payload_bytes, 4096);
            assert_eq!(send_result.trace_chunks, 25);
            assert_eq!(send_result.request_attempts, 2);
        }
    }

    /// Tests for serialization/deserialization metric constants
    mod serialization_metrics {
        use super::*;

        #[test]
        fn test_serialize_errors_constant_defined() {
            assert_eq!(
                SERIALIZE_TRACES_ERRORS,
                "datadog.tracer.exporter.serialize.errors"
            );
        }

        #[test]
        fn test_deserialize_errors_constant_defined() {
            assert_eq!(
                DESERIALIZE_TRACES_ERRORS,
                "datadog.tracer.exporter.deserialize.errors"
            );
        }

        #[test]
        fn test_serialize_metric_can_be_used() {
            let metric = HealthMetric::Count(SERIALIZE_TRACES_ERRORS, 1);
            match metric {
                HealthMetric::Count(name, count) => {
                    assert_eq!(name, SERIALIZE_TRACES_ERRORS);
                    assert_eq!(count, 1);
                }
                _ => panic!("Expected Count metric"),
            }
        }
    }
}
