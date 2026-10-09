// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Measurements of one trace payload delivery, shared by native telemetry and, when enabled,
//! externally drained observations.

use libdd_trace_utils::{
    send_with_retry::{SendWithRetryError, SendWithRetryResult},
    trace_utils::SendDataResult,
};
use std::collections::HashMap;

/// Measurements from one completed payload operation, including failed operations.
///
/// Requests count attempts, whereas responses and errors describe terminal results only.
/// `bytes_sent` is one distribution sample, not a counter: do not sum reports before
/// recording it. Zero values are not emitted. Counts describe chunks and spans after
/// filtering, rather than the input batch. No timestamp or SDK language is attached;
/// the consuming telemetry client owns aggregation and the application envelope.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SendPayloadTelemetry {
    pub requests_count: u64,
    pub errors_network: u64,
    pub errors_timeout: u64,
    pub errors_status_code: u64,
    pub bytes_sent: u64,
    pub chunks_sent: u64,
    pub chunks_dropped_serialization_error: u64,
    pub chunks_dropped_send_failure: u64,
    pub spans_enqueued_for_serialization: u64,
    pub spans_dropped_serialization_error: u64,
    pub spans_dropped_api_error: u64,
    pub responses_count_per_code: HashMap<u16, u64>,
}

impl From<&SendDataResult> for SendPayloadTelemetry {
    fn from(value: &SendDataResult) -> Self {
        Self {
            requests_count: value.requests_count,
            errors_network: value.errors_network,
            errors_timeout: value.errors_timeout,
            errors_status_code: value.errors_status_code,
            bytes_sent: value.bytes_sent,
            chunks_sent: value.chunks_sent,
            chunks_dropped_send_failure: value.chunks_dropped,
            responses_count_per_code: value.responses_count_per_code.clone(),
            ..Default::default()
        }
    }
}

impl SendPayloadTelemetry {
    /// Convert a terminal retry result, using the producer's payload size and chunk count.
    pub fn from_retry_result(value: &SendWithRetryResult, bytes_sent: u64, chunks: u64) -> Self {
        let mut telemetry = Self::default();
        match value {
            Ok((response, attempts)) => {
                telemetry.chunks_sent = chunks;
                telemetry.bytes_sent = bytes_sent;
                telemetry
                    .responses_count_per_code
                    .insert(response.status().as_u16(), 1);
                telemetry.requests_count = u64::from(*attempts);
            }
            Err(err) => match err {
                SendWithRetryError::Http(response, attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_status_code = 1;
                    telemetry
                        .responses_count_per_code
                        .insert(response.status().as_u16(), 1);
                    telemetry.requests_count = u64::from(*attempts);
                }
                SendWithRetryError::Timeout(attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_timeout = 1;
                    telemetry.requests_count = u64::from(*attempts);
                }
                SendWithRetryError::Network(_, attempts)
                | SendWithRetryError::ResponseBody(attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_network = 1;
                    telemetry.requests_count = u64::from(*attempts);
                }
                SendWithRetryError::Build(attempts) => {
                    telemetry.chunks_dropped_serialization_error = chunks;
                    telemetry.requests_count = u64::from(*attempts);
                }
            },
        }
        telemetry
    }

    pub(crate) fn from_retry_result_with_spans(
        value: &SendWithRetryResult,
        bytes_sent: u64,
        chunks: u64,
        spans: u64,
    ) -> Self {
        let mut telemetry = Self::from_retry_result(value, bytes_sent, chunks);
        telemetry.spans_enqueued_for_serialization = spans;
        match value {
            Err(SendWithRetryError::Build(_)) => {
                telemetry.spans_dropped_serialization_error = spans
            }
            Err(_) => telemetry.spans_dropped_api_error = spans,
            Ok(_) => {}
        }
        telemetry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_preserve_attempts_and_distinguish_drop_reasons() {
        for (error, expected) in [
            (SendWithRetryError::Build(0), (0, 0, 4, 0)),
            (SendWithRetryError::Build(1), (0, 0, 4, 0)),
            (SendWithRetryError::Timeout(3), (0, 1, 0, 4)),
            (SendWithRetryError::ResponseBody(3), (1, 0, 0, 4)),
        ] {
            let report = SendPayloadTelemetry::from_retry_result_with_spans(&Err(error), 123, 2, 4);
            assert_eq!(
                (
                    report.errors_network,
                    report.errors_timeout,
                    report.spans_dropped_serialization_error,
                    report.spans_dropped_api_error
                ),
                expected
            );
            assert_eq!(report.spans_enqueued_for_serialization, 4);
            assert_eq!(report.bytes_sent, 0);
            assert_eq!(report.chunks_sent, 0);
            assert!(report.responses_count_per_code.is_empty());
        }
    }
}
