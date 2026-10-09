// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C bindings for external observations: exporter measurements returned to the caller
//! instead of being delivered by the native telemetry client.
//!
//! This whole module is the `external-observations` feature and is meant to be deleted as a
//! unit, together with `ddog_trace_exporter_config_enable_observations`.

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use crate::trace_exporter::TraceExporter;
use crate::tracer::TracerTraceChunks;
use crate::{catch_panic, gen_error};
use libdd_trace_utils::span::span_pool::PooledChunks;
use std::ptr::NonNull;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

/// Caller-owned observations from one send. All counts are unsigned and zero-suppressed.
/// `bytes_sent` is one successful payload-size distribution sample. Status zero means no
/// terminal HTTP response; otherwise `responses_count` belongs to `status_code`.
/// The struct contains no owned pointers and requires no destructor.
#[repr(C)]
#[derive(Debug, Default)]
pub struct TraceExporterObservations {
    pub requests_count: u64,
    pub errors_network: u64,
    pub errors_timeout: u64,
    pub errors_status_code: u64,
    pub bytes_sent: u64,
    pub chunks_sent: u64,
    pub chunks_dropped_serialization_error: u64,
    pub chunks_dropped_send_failure: u64,
    pub chunks_dropped_p0: u64,
    pub chunks_dropped_by_trace_filter: u64,
    pub spans_enqueued_for_serialization: u64,
    pub spans_dropped_serialization_error: u64,
    pub spans_dropped_api_error: u64,
    pub responses_count: u64,
    pub status_code: u16,
}

impl From<libdd_data_pipeline::trace_exporter::observations::SendObservations>
    for TraceExporterObservations
{
    fn from(report: libdd_data_pipeline::trace_exporter::observations::SendObservations) -> Self {
        let payload = report.payload.unwrap_or_default();
        let (status_code, responses_count) = payload
            .responses_count_per_code
            .into_iter()
            .next()
            .unwrap_or_default();
        Self {
            requests_count: payload.requests_count,
            errors_network: payload.errors_network,
            errors_timeout: payload.errors_timeout,
            errors_status_code: payload.errors_status_code,
            bytes_sent: payload.bytes_sent,
            chunks_sent: payload.chunks_sent,
            chunks_dropped_serialization_error: payload.chunks_dropped_serialization_error,
            chunks_dropped_send_failure: payload.chunks_dropped_send_failure,
            chunks_dropped_p0: report.chunks_dropped_p0,
            chunks_dropped_by_trace_filter: report.chunks_dropped_by_trace_filter,
            spans_enqueued_for_serialization: payload.spans_enqueued_for_serialization,
            spans_dropped_serialization_error: payload.spans_dropped_serialization_error,
            spans_dropped_api_error: payload.spans_dropped_api_error,
            responses_count,
            status_code,
        }
    }
}

/// Send chunks with caller-owned observations on success, failure, or cancellation.
/// Native telemetry is not submitted for this operation. Both outputs are initialised
/// before validation; invalid input and contained panics leave zero observations.
/// `chunks` is consumed on every return, including invalid arguments and panics.
///
/// # Safety
/// Non-null pointers must be valid for their types. Output slots must be writable,
/// unaliased, and contain no live owned response. Free a returned response with
/// `ddog_trace_exporter_response_free`; observations require no cleanup.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_send_trace_chunks_observed(
    exporter: Option<&TraceExporter>,
    chunks: Option<Box<TracerTraceChunks>>,
    response_out: Option<NonNull<Option<Box<ExporterResponse>>>>,
    observations_out: Option<NonNull<TraceExporterObservations>>,
    cancel: Option<&TokioCancellationToken>,
) -> Option<Box<ExporterError>> {
    if let Some(out) = response_out {
        unsafe {
            out.as_ptr().write(None);
        }
    }
    let Some(mut observations_out) = observations_out else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    unsafe {
        observations_out
            .as_ptr()
            .write(TraceExporterObservations::default());
    }
    let observations_out = unsafe { observations_out.as_mut() };
    catch_panic!(
        {
            let (Some(exporter), Some(chunks)) = (exporter, chunks) else {
                return gen_error!(ErrorCode::InvalidArgument);
            };
            let (result, report) =
                exporter.send_trace_chunks_observed(PooledChunks::unpooled(chunks.0), cancel);
            *observations_out = report.into();
            match result {
                Ok(response) => {
                    if let Some(out) = response_out {
                        unsafe {
                            out.as_ptr()
                                .write(Some(Box::new(ExporterResponse::from(response))));
                        }
                    }
                    None
                }
                Err(error) => Some(Box::new(ExporterError::from(error))),
            }
        },
        {
            *observations_out = TraceExporterObservations::default();
            gen_error!(ErrorCode::Panic)
        }
    )
}

/// Bounded stats counters: slot zero is whole-key collapse, slots 1..16 are bitmasks
/// of resource (1), HTTP endpoint (2), peer tags (4), and additional metric tags (8).
#[repr(C)]
#[derive(Debug, Default)]
pub struct TraceExporterStatsObservations {
    pub collapsed_spans: [u64; 16],
}

/// Atomically drain background stats counters. Concurrent readers consume disjoint deltas.
/// Drain and discard inherited counters in a forked child before restarting workers.
///
/// # Safety
/// Non-null pointers must be valid; `out` must be writable and unaliased.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_take_stats_observations(
    exporter: Option<&TraceExporter>,
    out: Option<NonNull<TraceExporterStatsObservations>>,
) -> Option<Box<ExporterError>> {
    let Some(mut out) = out else {
        return gen_error!(ErrorCode::InvalidArgument);
    };
    unsafe {
        out.as_ptr()
            .write(TraceExporterStatsObservations::default());
    }
    let out = unsafe { out.as_mut() };
    catch_panic!(
        if let Some(exporter) = exporter {
            out.collapsed_spans = exporter.take_stats_observations();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Consume an exporter, stop its workers, and return their final stats observations.
/// A null output discards observations while still shutting down the exporter.
///
/// # Safety
/// `exporter` must be exclusively owned and must not be used again. A non-null `out`
/// must be writable and unaliased. It is initialised even on invalid input or panic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_shutdown_observed(
    exporter: Option<Box<TraceExporter>>,
    out: Option<NonNull<TraceExporterStatsObservations>>,
) -> Option<Box<ExporterError>> {
    if let Some(out) = out {
        unsafe {
            out.as_ptr()
                .write(TraceExporterStatsObservations::default());
        }
    }
    catch_panic!(
        if let Some(exporter) = exporter {
            let (result, counts) = exporter.shutdown_observed(None);
            if let Some(out) = out {
                unsafe {
                    out.as_ptr().write(TraceExporterStatsObservations {
                        collapsed_spans: counts,
                    });
                }
            }
            result.err().map(|e| Box::new(ExporterError::from(e)))
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn observation_outputs_are_initialised_on_invalid_arguments() {
        unsafe {
            let mut response = MaybeUninit::<Option<Box<ExporterResponse>>>::uninit();
            let mut report = MaybeUninit::<TraceExporterObservations>::uninit();
            let error = ddog_trace_exporter_send_trace_chunks_observed(
                None,
                None,
                NonNull::new(response.as_mut_ptr()),
                NonNull::new(report.as_mut_ptr()),
                None,
            );
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);
            assert!(response.assume_init().is_none());
            let report = report.assume_init();
            assert_eq!(report.requests_count, 0);
            assert_eq!(report.status_code, 0);
            assert_eq!(report.bytes_sent, 0);

            let mut stats = MaybeUninit::<TraceExporterStatsObservations>::uninit();
            let error =
                ddog_trace_exporter_take_stats_observations(None, NonNull::new(stats.as_mut_ptr()));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);
            assert_eq!(stats.assume_init().collapsed_spans, [0; 16]);
        }
    }
}
