// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless HTTP/JSON trace exporter.

use super::config::AgentlessTraceConfig;
use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use http::HeaderMap;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryBackoffType, RetryStrategy, SendWithRetryError,
};
use tracing::error;

const AGENTLESS_MAX_RETRIES: u32 = 2;
const AGENTLESS_RETRY_DELAY_MS: u64 = 1000;

/// Send an agentless trace payload (JSON bytes) to the configured intake with retries.
///
/// `headers` should already contain all required headers (api key, content-type, meta-*,
/// entity, trace-count, etc.). `test_token` is forwarded as `X-Datadog-Test-Session-Token`
/// when set, enabling snapshot tests against a local mock.
pub async fn send_agentless_traces_http<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    config: &AgentlessTraceConfig,
    headers: HeaderMap,
    json_body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    let url = libdd_common::parse_uri(&config.endpoint_url).map_err(|e| {
        TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
            "Invalid agentless endpoint URL: {e}"
        )))
    })?;

    let target = Endpoint {
        url,
        timeout_ms: config.timeout.as_millis() as u64,
        ..Endpoint::default()
    };

    let retry_strategy = RetryStrategy::new(
        AGENTLESS_MAX_RETRIES,
        AGENTLESS_RETRY_DELAY_MS,
        RetryBackoffType::Exponential,
        None,
    );

    match send_with_retry(capabilities, &target, json_body, &headers, &retry_strategy).await {
        Ok(_) => Ok(()),
        Err(e) => Err(map_send_error(e)),
    }
}

fn map_send_error(err: SendWithRetryError) -> TraceExporterError {
    match err {
        SendWithRetryError::Http(response, _) => {
            let status = response.status();
            let body_str = String::from_utf8_lossy(response.body());
            match status.as_u16() {
                401 | 403 => error!(
                    status = status.as_u16(),
                    body = %body_str,
                    "Agentless authentication failed. Verify DD_API_KEY is valid."
                ),
                404 => error!(
                    status = status.as_u16(),
                    body = %body_str,
                    "Agentless endpoint not found. Verify DD_SITE is correctly configured."
                ),
                429 => error!(
                    status = status.as_u16(),
                    body = %body_str,
                    "Agentless intake rate-limited the request. Traces were dropped."
                ),
                500..=599 => error!(
                    status = status.as_u16(),
                    body = %body_str,
                    "Agentless intake returned a server error. Traces were dropped."
                ),
                _ => error!(
                    status = status.as_u16(),
                    body = %body_str,
                    "Agentless intake returned an unexpected status."
                ),
            }
            TraceExporterError::Request(RequestError::new(status, &body_str))
        }
        SendWithRetryError::Timeout(_) => {
            TraceExporterError::Io(std::io::Error::from(std::io::ErrorKind::TimedOut))
        }
        SendWithRetryError::Network(error, _) => TraceExporterError::from(error),
        SendWithRetryError::ResponseBody(_) => {
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(
                "Failed to read agentless response body".to_string(),
            ))
        }
        SendWithRetryError::Build(_) => TraceExporterError::Internal(
            InternalErrorKind::InvalidWorkerState("Failed to build agentless request".to_string()),
        ),
    }
}
