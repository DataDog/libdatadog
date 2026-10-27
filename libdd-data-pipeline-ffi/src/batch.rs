// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Owned batch results. Accessors borrow from the batch; one destructor releases
//! all response bodies and errors, including results not visited by the caller.

use crate::error::ExporterErrorCode;
use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::trace_exporter::TraceExporter;
use crate::tracer::TracerTraceChunks;
use crate::{catch_panic, gen_error};
use libdd_common_ffi::slice::ByteSlice;
use libdd_data_pipeline::trace_exporter::{
    agent_response::AgentResponse, batch::BatchResult, observations::TraceExporterObservations,
};
use libdd_trace_utils::span::span_pool::PooledChunks;
use std::ptr::NonNull;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

struct PayloadResult {
    chunks: usize,
    spans: usize,
    body: Option<String>,
    error: Option<ExporterErrorCode>,
    observations: TraceExporterObservations,
}

/// Owns every outcome of one bounded send. Free with `ddog_trace_exporter_batch_free`.
pub struct TraceExporterBatch {
    payloads: Vec<PayloadResult>,
    summary: TraceExporterBatchSummary,
}

/// Operation-level status and work without completed outcomes. `operation_failed`
/// excludes individual payload failures and oversized drops. Inspect payload views
/// separately; their count includes local serialisation failures.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TraceExporterBatchSummary {
    pub payload_count: usize,
    pub oversized_chunks: usize,
    pub oversized_spans: usize,
    pub unattempted_chunks: usize,
    pub unattempted_spans: usize,
    pub in_flight_chunks: usize,
    pub in_flight_spans: usize,
    /// Counts not assigned to a completed send attempt, not delivery acknowledgements.
    pub pending_p0_traces: u64,
    pub pending_p0_spans: u64,
    pub operation_failed: bool,
    /// The operation stopped because of cancellation or its deadline, not setup failure.
    pub operation_cancelled: bool,
    pub error_code: ExporterErrorCode,
}

impl Default for TraceExporterBatchSummary {
    fn default() -> Self {
        Self {
            payload_count: 0,
            oversized_chunks: 0,
            oversized_spans: 0,
            unattempted_chunks: 0,
            unattempted_spans: 0,
            in_flight_chunks: 0,
            in_flight_spans: 0,
            pending_p0_traces: 0,
            pending_p0_spans: 0,
            operation_failed: false,
            operation_cancelled: false,
            error_code: ErrorCode::HttpUnknown,
        }
    }
}

/// Borrowed payload view, valid only while its batch remains alive.
#[repr(C)]
pub struct TraceExporterBatchPayload<'a> {
    pub present: bool,
    pub chunks: usize,
    pub spans: usize,
    pub failed: bool,
    pub error_code: ExporterErrorCode,
    pub body: ByteSlice<'a>,
    pub observations: TraceExporterObservations,
}

impl Default for TraceExporterBatchPayload<'_> {
    fn default() -> Self {
        Self {
            present: false,
            chunks: 0,
            spans: 0,
            failed: false,
            error_code: ErrorCode::HttpUnknown,
            body: ByteSlice::from(&[][..]),
            observations: Default::default(),
        }
    }
}

impl From<BatchResult> for TraceExporterBatch {
    fn from(result: BatchResult) -> Self {
        let error = result.error.as_ref().map(ExporterErrorCode::from);
        let summary = TraceExporterBatchSummary {
            payload_count: result.payloads.len(),
            oversized_chunks: result.oversized_chunks,
            oversized_spans: result.oversized_spans,
            unattempted_chunks: result.unattempted_chunks,
            unattempted_spans: result.unattempted_spans,
            in_flight_chunks: result.in_flight_chunks,
            in_flight_spans: result.in_flight_spans,
            pending_p0_traces: result.pending_p0_traces,
            pending_p0_spans: result.pending_p0_spans,
            operation_failed: error.is_some(),
            operation_cancelled: matches!(&result.error,
                Some(libdd_data_pipeline::trace_exporter::error::TraceExporterError::Io(error))
                    if matches!(error.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::TimedOut)),
            error_code: error.unwrap_or(ErrorCode::HttpUnknown),
        };
        let payloads = result
            .payloads
            .into_iter()
            .map(|payload| {
                let (body, error) = match payload.response {
                    Ok(AgentResponse::Changed { body }) => (Some(body), None),
                    Ok(AgentResponse::Unchanged) => (None, None),
                    Err(error) => (None, Some(ExporterErrorCode::from(&error))),
                };
                PayloadResult {
                    chunks: payload.chunks,
                    spans: payload.spans,
                    body,
                    error,
                    observations: payload.observations,
                }
            })
            .collect();
        Self { payloads, summary }
    }
}

/// Send bounded Agent payloads. Zero `max_payload_bytes` selects the native default.
/// Input chunks are consumed on every return. Payload failures are returned in the
/// batch; the function returns an error only if it cannot produce a batch result.
/// The caller owns observation delivery; native payload telemetry is bypassed.
/// Only the latest usable sampling response retains its body. Error views expose
/// codes and HTTP status observations, not allocated diagnostic strings.
///
/// # Safety
/// All non-null pointers must be valid. `out` must be writable, unaliased, and not
/// contain a live owned batch. Free the returned batch even after cancellation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_send_trace_chunks_batched(
    exporter: Option<&TraceExporter>,
    chunks: Option<Box<TracerTraceChunks>>,
    max_payload_bytes: usize,
    out: Option<NonNull<Option<Box<TraceExporterBatch>>>>,
    cancel: Option<&TokioCancellationToken>,
) -> Option<Box<ExporterError>> {
    let Some(out) = out else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    unsafe {
        out.as_ptr().write(None);
    }
    catch_panic!(
        {
            let (Some(exporter), Some(chunks)) = (exporter, chunks) else {
                return gen_error!(ErrorCode::InvalidArgument);
            };
            let result = exporter.send_trace_chunks_batched(
                PooledChunks::unpooled(chunks.0),
                max_payload_bytes,
                cancel,
            );
            unsafe {
                out.as_ptr().write(Some(Box::new(result.into())));
            }
            None
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Get batch-level outcomes. Null yields an empty summary.
///
/// # Safety
/// A non-null batch must remain alive for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_batch_summary(
    batch: Option<&TraceExporterBatch>,
) -> TraceExporterBatchSummary {
    batch.map_or_else(Default::default, |batch| batch.summary)
}

/// Get a borrowed payload view. An invalid index or null batch has `present=false`.
///
/// # Safety
/// A non-null batch must remain alive while the returned body is used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_batch_payload(
    batch: Option<&TraceExporterBatch>,
    index: usize,
) -> TraceExporterBatchPayload<'_> {
    batch
        .and_then(|batch| batch.payloads.get(index))
        .map_or_else(Default::default, |payload| TraceExporterBatchPayload {
            present: true,
            chunks: payload.chunks,
            spans: payload.spans,
            failed: payload.error.is_some(),
            error_code: payload.error.unwrap_or(ErrorCode::HttpUnknown),
            body: ByteSlice::from(payload.body.as_deref().unwrap_or_default().as_bytes()),
            observations: payload.observations,
        })
}

/// Release a batch and all its borrowed views. Null is accepted.
///
/// # Safety
/// `batch` must be null or an owned pointer returned by the batched send, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_batch_free(batch: Option<Box<TraceExporterBatch>>) {
    drop(batch);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_distinct_from_failed_setup() {
        for (kind, cancelled) in [
            (std::io::ErrorKind::Interrupted, true),
            (std::io::ErrorKind::TimedOut, true),
            (std::io::ErrorKind::Other, false),
        ] {
            let batch = TraceExporterBatch::from(BatchResult {
                error: Some(std::io::Error::from(kind).into()),
                unattempted_chunks: 1,
                unattempted_spans: 2,
                ..Default::default()
            });
            assert!(batch.summary.operation_failed);
            assert_eq!(batch.summary.operation_cancelled, cancelled);
            assert_eq!(batch.summary.unattempted_chunks, 1);
            assert_eq!(batch.summary.unattempted_spans, 2);
        }
        assert!(
            !TraceExporterBatch::from(BatchResult::default())
                .summary
                .operation_cancelled
        );
    }
    use libdd_data_pipeline::trace_exporter::{
        batch::PayloadResult as NativePayload,
        error::{RequestError, TraceExporterError},
    };

    #[test]
    fn views_preserve_independent_results_and_validate_indices() {
        let batch = Box::new(TraceExporterBatch::from(BatchResult {
            payloads: vec![
                NativePayload {
                    chunks: 2,
                    spans: 3,
                    response: Ok(AgentResponse::Changed {
                        body: "rates".to_owned(),
                    }),
                    observations: TraceExporterObservations {
                        requests_count: 1,
                        status_code: 200,
                        ..Default::default()
                    },
                },
                NativePayload {
                    chunks: 1,
                    spans: 1,
                    response: Err(TraceExporterError::Request(RequestError::new(
                        503.try_into().unwrap(),
                        "unavailable",
                    ))),
                    observations: TraceExporterObservations {
                        requests_count: 2,
                        status_code: 503,
                        ..Default::default()
                    },
                },
            ],
            oversized_chunks: 1,
            oversized_spans: 7,
            ..Default::default()
        }));
        unsafe {
            let summary = ddog_trace_exporter_batch_summary(Some(&batch));
            assert_eq!((summary.payload_count, summary.oversized_chunks), (2, 1));
            let first = ddog_trace_exporter_batch_payload(Some(&batch), 0);
            assert!(first.present && !first.failed);
            assert_eq!(first.chunks, 2);
            assert_eq!(first.body.as_slice(), b"rates");
            let second = ddog_trace_exporter_batch_payload(Some(&batch), 1);
            assert!(second.present && second.failed);
            assert_eq!(second.error_code, ErrorCode::HttpServer);
            assert_eq!(second.observations.status_code, 503);
            assert!(!ddog_trace_exporter_batch_payload(Some(&batch), 2).present);
            assert!(!ddog_trace_exporter_batch_payload(None, 0).present);
            ddog_trace_exporter_batch_free(Some(batch));
            ddog_trace_exporter_batch_free(None);
        }
    }

    #[test]
    fn invalid_send_initialises_the_result_slot() {
        let mut slot = std::ptr::dangling_mut::<TraceExporterBatch>();
        let out = NonNull::from(&mut slot).cast::<Option<Box<TraceExporterBatch>>>();
        let error = unsafe {
            ddog_trace_exporter_send_trace_chunks_batched(None, None, 0, Some(out), None)
        };
        assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);
        assert!(slot.is_null());
    }
}
