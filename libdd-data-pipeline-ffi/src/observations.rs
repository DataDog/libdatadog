// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C bindings for external observations: exporter measurements returned to the caller
//! instead of being delivered by the native telemetry client.
//!
//! This whole module is the `external-observations` feature.

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use crate::trace_exporter::{TraceExporter, TraceExporterConfig};
use crate::tracer::TracerTraceChunks;
use crate::{catch_panic, gen_error};
use libdd_data_pipeline::trace_exporter::observations::TraceExporterObservations;
use libdd_trace_utils::span::span_pool::PooledChunks;
use std::ptr::NonNull;
use tokio_util::sync::CancellationToken as TokioCancellationToken;

/// Select external observation delivery and disable native telemetry delivery.
///
/// # Safety
/// `config`, when non-null, must be a live, exclusively borrowed configuration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_config_enable_observations(
    config: Option<&mut TraceExporterConfig>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(config) = config {
            config.observations = true;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
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
            *observations_out = report;
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
