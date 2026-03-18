// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON trace exporter.

use super::config::OtlpTraceConfig;
use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use http::HeaderMap;
use libdd_common::{http_common, Endpoint, HttpClient};
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryBackoffType, RetryStrategy, SendWithRetryError,
};

/// Max total attempts for OTLP export (1 initial + up to 4 retries on transient failures).
const OTLP_MAX_ATTEMPTS: u32 = 5;
/// Initial backoff between retries (milliseconds).
const OTLP_RETRY_DELAY_MS: u64 = 100;

/// Send OTLP trace payload (JSON bytes) to the configured endpoint with retries.
///
/// Uses [`send_with_retry`] for consistent retry behaviour and observability across exporters.
///
/// `test_token` is forwarded as `X-Datadog-Test-Session-Token` when set, enabling snapshot tests
/// against the Datadog test agent's OTLP endpoint.
pub async fn send_otlp_traces_http(
    client: &HttpClient,
    config: &OtlpTraceConfig,
    test_token: Option<&str>,
    json_body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    let url = libdd_common::parse_uri(&config.endpoint_url).map_err(|e| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid OTLP endpoint URL: {}",
            e
        )))
    })?;

    let target = Endpoint {
        url,
        timeout_ms: config.timeout.as_millis() as u64,
        ..Endpoint::default()
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    for (key, value) in &config.headers {
        if let (Ok(name), Ok(val)) = (
            http::HeaderName::from_bytes(key.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            headers.insert(name, val);
        }
    }
    if let Some(token) = test_token {
        if let Ok(val) = http::HeaderValue::from_str(token) {
            headers.insert(
                http::HeaderName::from_static("x-datadog-test-session-token"),
                val,
            );
        }
    }

    let retry_strategy = RetryStrategy::new(
        OTLP_MAX_ATTEMPTS,
        OTLP_RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );

    match send_with_retry(client, &target, json_body, &headers, &retry_strategy).await {
        Ok(_) => Ok(()),
        Err(e) => Err(map_send_error(e).await),
    }
}

async fn map_send_error(err: SendWithRetryError) -> TraceExporterError {
    match err {
        SendWithRetryError::Http(response, _) => {
            let status = response.status();
            let body_bytes = http_common::collect_response_bytes(response)
                .await
                .unwrap_or_default();
            let body_str = String::from_utf8_lossy(&body_bytes);
            TraceExporterError::Request(RequestError::new(status, &body_str))
        }
        SendWithRetryError::Timeout(_) => {
            TraceExporterError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut))
        }
        SendWithRetryError::Network(error, _) => TraceExporterError::from(error),
        SendWithRetryError::Build(_) => TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState("Failed to build OTLP request".to_string()),
        ),
    }
}
