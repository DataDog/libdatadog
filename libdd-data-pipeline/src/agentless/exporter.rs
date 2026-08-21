// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Executor-owned adapter for agentless trace export.

use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use http::HeaderMap;
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_data_pipeline_core::{
    prepare_agentless_json_request, prepare_agentless_traces_request as prepare_traces,
    AgentlessTraceConfig, PrepareAgentlessError, PreparedAgentlessRequest,
};
use libdd_trace_utils::send_with_retry::{send_prepared_with_retry, SendWithRetryError};
use libdd_trace_utils::span::TraceData;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use tracing::error;

pub(crate) fn prepare_agentless_traces_request<T: TraceData>(
    traces: Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    metadata: &TracerMetadata,
    config: &AgentlessTraceConfig,
) -> Result<PreparedAgentlessRequest, TraceExporterError> {
    prepare_traces(traces, metadata, config).map_err(map_prepare_error)
}

/// Sends an encoded agentless JSON request using executor-provided capabilities.
pub async fn send_agentless_traces_http<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    config: &AgentlessTraceConfig,
    headers: HeaderMap,
    json_body: Vec<u8>,
) -> Result<(), TraceExporterError> {
    let prepared =
        prepare_agentless_json_request(config, headers, json_body).map_err(map_prepare_error)?;
    send_prepared_agentless_request(capabilities, &prepared).await
}

pub(crate) async fn send_prepared_agentless_request<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    prepared: &PreparedAgentlessRequest,
) -> Result<(), TraceExporterError> {
    send_prepared_with_retry(
        capabilities,
        prepared.request_plan(),
        prepared.retry_strategy(),
    )
    .await
    .map(|_| ())
    .map_err(map_send_error)
}

fn map_prepare_error(error: PrepareAgentlessError) -> TraceExporterError {
    match error {
        PrepareAgentlessError::Deserialization(error) => TraceExporterError::Deserialization(error),
        error => {
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(error.to_string()))
        }
    }
}

fn map_send_error(error: SendWithRetryError) -> TraceExporterError {
    match error {
        SendWithRetryError::Http(response, _) => {
            let status = response.status();
            let body = String::from_utf8_lossy(response.body());
            match status.as_u16() {
                401 | 403 => error!(
                    status = status.as_u16(),
                    body = %body,
                    "Agentless authentication failed. Verify DD_API_KEY is valid."
                ),
                404 => error!(
                    status = status.as_u16(),
                    body = %body,
                    "Agentless endpoint not found. Verify DD_SITE is correctly configured."
                ),
                429 => error!(
                    status = status.as_u16(),
                    body = %body,
                    "Agentless intake rate-limited the request. Traces were dropped."
                ),
                500..=599 => error!(
                    status = status.as_u16(),
                    body = %body,
                    "Agentless intake returned a server error. Traces were dropped."
                ),
                _ => error!(
                    status = status.as_u16(),
                    body = %body,
                    "Agentless intake returned an unexpected status."
                ),
            }
            TraceExporterError::Request(RequestError::new(status, &body))
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
