// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless trace export error adapter.

use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_data_pipeline_core::{
    send_agentless_traces_with_observer as send_traces, AgentlessError, AgentlessTraceConfig,
};
use libdd_trace_utils::send_with_retry::{SendWithRetryError, SendWithRetryResult};
use libdd_trace_utils::span::span_pool::PooledChunks;
use libdd_trace_utils::span::TraceData;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use tracing::error;

pub(crate) async fn send_agentless_traces_with_observer<T, C, F, S>(
    capabilities: &C,
    traces: PooledChunks<'_, T>,
    metadata: &TracerMetadata,
    config: &AgentlessTraceConfig,
    client_side_stats: bool,
    observer: F,
    serialization_error_observer: S,
) -> Result<(), TraceExporterError>
where
    T: TraceData,
    C: HttpClientCapability + SleepCapability,
    F: FnOnce(&SendWithRetryResult, usize),
    S: FnOnce(),
{
    let result = send_traces(
        capabilities,
        traces,
        metadata,
        config,
        client_side_stats,
        observer,
    )
    .await;
    if matches!(&result, Err(AgentlessError::Serialization(_))) {
        serialization_error_observer();
    }
    result.map_err(map_agentless_error)
}

fn map_agentless_error(error: AgentlessError) -> TraceExporterError {
    match error {
        AgentlessError::Send(error) => map_send_error(*error),
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
