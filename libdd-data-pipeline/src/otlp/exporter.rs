// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON trace exporter.

use super::config::OtlpTraceConfig;
use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use http::HeaderMap;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryBackoffType, RetryStrategy, SendWithRetryError,
};
use std::time::Duration;

/// Max total attempts for OTLP export (initial + retries on transient failures).
pub(crate) const OTLP_MAX_ATTEMPTS: u32 = 5;
/// Single attempt with no retries, used on shutdown to avoid a long backoff in the shutdown window.
pub(crate) const OTLP_SHUTDOWN_MAX_ATTEMPTS: u32 = 1;
const OTLP_RETRY_DELAY_MS: u64 = 100;

/// POST an OTLP HTTP/JSON payload to `endpoint_url`; `test_token` enables snapshot tests.
pub(crate) async fn send_otlp_http<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    endpoint_url: &str,
    config_headers: &HeaderMap,
    timeout: Duration,
    test_token: Option<&str>,
    json_body: Vec<u8>,
    max_attempts: u32,
) -> Result<(), TraceExporterError> {
    let url = libdd_common::parse_uri(endpoint_url).map_err(|e| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid OTLP endpoint URL: {}",
            e
        )))
    })?;

    let target = Endpoint {
        url,
        timeout_ms: timeout.as_millis() as u64,
        ..Endpoint::default()
    };

    let mut headers = config_headers.clone();
    headers.insert(
        http::header::CONTENT_TYPE,
        libdd_common::header::APPLICATION_JSON,
    );
    if let Some(token) = test_token {
        if let Ok(val) = http::HeaderValue::from_str(token) {
            headers.insert(
                http::HeaderName::from_static("x-datadog-test-session-token"),
                val,
            );
        }
    }

    // `RetryStrategy` counts *retries*, and performs `max_retries + 1` total attempts. Convert the
    // attempt budget accordingly so `max_attempts == 1` means a single try with no retries.
    let retry_strategy = RetryStrategy::new(
        max_attempts.saturating_sub(1),
        OTLP_RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );

    match send_with_retry(capabilities, &target, json_body, &headers, &retry_strategy).await {
        Ok(_) => Ok(()),
        Err(e) => Err(map_send_error(e).await),
    }
}

/// Send OTLP trace payload (JSON bytes) to the configured endpoint with retries.
///
/// Uses [`send_with_retry`] for consistent retry behaviour and observability across exporters.
///
/// `test_token` is forwarded as `X-Datadog-Test-Session-Token` when set, enabling snapshot tests
/// against the Datadog test agent's OTLP endpoint.
pub async fn send_otlp_traces_http<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    config: &OtlpTraceConfig,
    test_token: Option<&str>,
    json_body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    send_otlp_http(
        capabilities,
        &config.endpoint_url,
        &config.headers,
        config.timeout,
        test_token,
        json_body,
        OTLP_MAX_ATTEMPTS,
    )
    .await
}

async fn map_send_error(err: SendWithRetryError) -> TraceExporterError {
    match err {
        SendWithRetryError::Http(response, _) => {
            let status = response.status();
            let body_str = String::from_utf8_lossy(response.body());
            TraceExporterError::Request(RequestError::new(status, &body_str))
        }
        SendWithRetryError::Timeout(_) => {
            TraceExporterError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut))
        }
        SendWithRetryError::Network(error, _) => TraceExporterError::from(error),
        SendWithRetryError::ResponseBody(_) => TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState("Failed to read OTLP response body".to_string()),
        ),
        SendWithRetryError::Build(_) => TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState("Failed to build OTLP request".to_string()),
        ),
    }
}
