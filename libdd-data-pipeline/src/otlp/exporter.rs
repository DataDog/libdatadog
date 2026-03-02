// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON trace exporter. Sends ExportTraceServiceRequest with retries on 429, 502, 503, 504.

use super::config::OtlpTraceConfig;
use crate::trace_exporter::error::TraceExporterError;
use http::Method;
use libdd_common::http_common::{self, Body};
use libdd_common::HttpClient;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, warn};

/// Max retries for OTLP export (transient failures only).
const OTLP_MAX_RETRIES: u32 = 5;
/// Initial backoff between retries (milliseconds).
const OTLP_RETRY_DELAY_MS: u64 = 100;

/// Status codes that trigger a retry (transient).
fn is_retryable_status(code: u16) -> bool {
    matches!(code, 429 | 502 | 503 | 504)
}

/// Send OTLP trace payload (JSON bytes) to the configured endpoint.
///
/// Retries with exponential backoff only on 429, 502, 503, 504. Does not retry on 4xx (e.g. 400).
/// Uses the timeout from config.
pub async fn send_otlp_traces_http(
    client: &HttpClient,
    config: &OtlpTraceConfig,
    json_body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    let uri = libdd_common::parse_uri(&config.endpoint_url).map_err(|e| {
        TraceExporterError::Internal(
            crate::trace_exporter::error::InternalErrorKind::InvalidWorkerState(format!(
                "Invalid OTLP endpoint URL: {}",
                e
            )),
        )
    })?;

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let req_builder = build_request(&uri, config)?;
        let timeout = config.timeout;
        let body_bytes = bytes::Bytes::from(json_body.clone());

        debug!(
            attempt,
            url = %config.endpoint_url,
            "OTLP trace export attempt"
        );

        let req = req_builder
            .body(Body::from_bytes(body_bytes))
            .map_err(|e| {
                TraceExporterError::Internal(
                    crate::trace_exporter::error::InternalErrorKind::InvalidWorkerState(
                        e.to_string(),
                    ),
                )
            })?;

        match tokio::time::timeout(timeout, client.request(req)).await {
            Ok(Ok(response)) => {
                let status = response.status();
                if status.is_success() {
                    debug!(status = %status, "OTLP trace export succeeded");
                    return Ok(());
                }
                let code = status.as_u16();
                if is_retryable_status(code) && attempt < OTLP_MAX_RETRIES {
                    let delay_ms = OTLP_RETRY_DELAY_MS * (1 << (attempt - 1));
                    warn!(
                        status = %status,
                        attempt,
                        delay_ms,
                        "OTLP export transient failure, retrying"
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }
                let response = http_common::into_response(response);
                let body_bytes = http_common::collect_response_bytes(response)
                    .await
                    .unwrap_or_default();
                let body_str = String::from_utf8_lossy(&body_bytes);
                error!(
                    status = %status,
                    attempt,
                    body = %body_str,
                    "OTLP trace export failed"
                );
                return Err(TraceExporterError::Request(
                    crate::trace_exporter::error::RequestError::new(status, &body_str),
                ));
            }
            Ok(Err(e)) => {
                if attempt < OTLP_MAX_RETRIES {
                    let delay_ms = OTLP_RETRY_DELAY_MS * (1 << (attempt - 1));
                    warn!(error = ?e, attempt, "OTLP export network error, retrying");
                    sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }
                error!(error = ?e, attempt, "OTLP trace export failed after retries");
                return Err(TraceExporterError::from(http_common::into_error(e)));
            }
            Err(_) => {
                if attempt < OTLP_MAX_RETRIES {
                    let delay_ms = OTLP_RETRY_DELAY_MS * (1 << (attempt - 1));
                    warn!(attempt, "OTLP export timeout, retrying");
                    sleep(Duration::from_millis(delay_ms)).await;
                    continue;
                }
                error!(attempt, "OTLP trace export timed out after retries");
                return Err(TraceExporterError::from(std::io::Error::from(
                    std::io::ErrorKind::TimedOut,
                )));
            }
        }
    }
}

fn build_request(
    uri: &http::Uri,
    config: &OtlpTraceConfig,
) -> Result<http::request::Builder, TraceExporterError> {
    let mut builder = http::Request::builder()
        .method(Method::POST)
        .uri(uri.clone())
        .header("Content-Type", "application/json");
    for (k, v) in &config.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    Ok(builder)
}
