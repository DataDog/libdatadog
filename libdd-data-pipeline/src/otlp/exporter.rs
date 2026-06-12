// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON trace exporter.

use super::config::OtlpTraceConfig;
use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryBackoffType, RetryStrategy, SendWithRetryError,
};

/// Max retries for OTLP export.
const OTLP_MAX_RETRIES: u32 = 4;
/// Initial backoff between retries (milliseconds).
const OTLP_RETRY_DELAY_MS: u64 = 100;

/// Send an OTLP trace payload to the configured endpoint with retries.
///
/// The body encoding and `Content-Type` are selected from `config.protocol`.
///
/// Uses [`send_with_retry`] for consistent retry behaviour and observability across exporters.
///
/// `test_token` is forwarded as `X-Datadog-Test-Session-Token` when set, enabling snapshot tests
/// against the Datadog test agent's OTLP endpoint.
pub async fn send_otlp_traces_http<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    config: &OtlpTraceConfig,
    test_token: Option<&str>,
    body: Vec<u8>,
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

    let content_type = match config.protocol {
        crate::otlp::config::OtlpProtocol::HttpProtobuf => {
            libdd_common::header::APPLICATION_PROTOBUF
        }
        _ => libdd_common::header::APPLICATION_JSON,
    };

    let mut headers = config.headers.clone();
    headers.insert(http::header::CONTENT_TYPE, content_type);
    if let Some(token) = test_token {
        if let Ok(val) = http::HeaderValue::from_str(token) {
            headers.insert(
                http::HeaderName::from_static("x-datadog-test-session-token"),
                val,
            );
        }
    }

    let retry_strategy = RetryStrategy::new(
        OTLP_MAX_RETRIES,
        OTLP_RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );

    match send_with_retry(capabilities, &target, body, &headers, &retry_strategy).await {
        Ok(_) => Ok(()),
        Err(e) => Err(map_send_error(e).await),
    }
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
