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
    use crate::trace_exporter::{
        ddog_trace_exporter_config_free, ddog_trace_exporter_config_new,
        ddog_trace_exporter_config_set_url, ddog_trace_exporter_free, ddog_trace_exporter_new,
    };
    use httpmock::prelude::*;
    use libdd_common_ffi::CharSlice;
    use libdd_trace_utils::span::v04::SpanBytes;
    use std::mem::MaybeUninit;

    /// Build an exporter with observations enabled, pointing at `url`.
    unsafe fn observed_exporter(url: &str) -> Box<TraceExporter> {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());
            let mut config = config.assume_init();
            assert!(
                ddog_trace_exporter_config_set_url(Some(config.as_mut()), CharSlice::from(url))
                    .is_none()
            );
            assert!(
                ddog_trace_exporter_config_enable_observations(Some(config.as_mut())).is_none()
            );
            let mut exporter: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
            let error = ddog_trace_exporter_new(
                NonNull::new_unchecked(&mut exporter).cast(),
                Some(config.as_ref()),
            );
            assert!(error.is_none());
            ddog_trace_exporter_config_free(config);
            exporter.assume_init()
        }
    }

    fn one_span_chunks() -> Option<Box<TracerTraceChunks>> {
        Some(Box::new(TracerTraceChunks(vec![
            vec![SpanBytes::default()],
        ])))
    }

    #[test]
    fn enable_observations_requires_a_config_and_sets_the_flag() {
        unsafe {
            let error = ddog_trace_exporter_config_enable_observations(None);
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);

            let mut config = TraceExporterConfig::default();
            assert!(!config.observations);
            assert!(ddog_trace_exporter_config_enable_observations(Some(&mut config)).is_none());
            assert!(config.observations);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn observed_send_fills_the_report_and_response_on_success() {
        unsafe {
            let server = MockServer::start();
            let traces = server.mock(|when, then| {
                when.method(POST).path("/v0.4/traces");
                then.status(200).body("{}");
            });
            let exporter = observed_exporter(&server.base_url());

            let mut response = MaybeUninit::<Option<Box<ExporterResponse>>>::uninit();
            let mut report = MaybeUninit::<TraceExporterObservations>::uninit();
            let error = ddog_trace_exporter_send_trace_chunks_observed(
                Some(exporter.as_ref()),
                one_span_chunks(),
                NonNull::new(response.as_mut_ptr()),
                NonNull::new(report.as_mut_ptr()),
                None,
            );
            assert!(error.is_none());
            assert!(response.assume_init().is_some());
            let report = report.assume_init();
            traces.assert_calls(1);
            assert_eq!((report.status_code, report.responses_count), (200, 1));
            assert_eq!(report.requests_count, 1);
            assert_eq!(report.chunks_sent, 1);
            assert_eq!(report.spans_enqueued_for_serialization, 1);
            assert!(report.bytes_sent > 0);
            assert_eq!(report.errors_status_code, 0);
            assert_eq!(report.chunks_dropped_send_failure, 0);

            ddog_trace_exporter_free(exporter);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn observed_send_fills_the_report_when_the_send_fails() {
        unsafe {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST).path("/v0.4/traces");
                then.status(503).body("{}");
            });
            let exporter = observed_exporter(&server.base_url());

            let mut response = MaybeUninit::<Option<Box<ExporterResponse>>>::uninit();
            let mut report = MaybeUninit::<TraceExporterObservations>::uninit();
            let error = ddog_trace_exporter_send_trace_chunks_observed(
                Some(exporter.as_ref()),
                one_span_chunks(),
                NonNull::new(response.as_mut_ptr()),
                NonNull::new(report.as_mut_ptr()),
                None,
            );
            assert!(error.is_some());
            assert!(response.assume_init().is_none());
            let report = report.assume_init();
            assert_eq!((report.status_code, report.responses_count), (503, 1));
            assert!(report.requests_count > 1);
            assert_eq!(report.errors_status_code, 1);
            assert_eq!(report.chunks_sent, 0);
            assert_eq!(report.chunks_dropped_send_failure, 1);
            assert_eq!(report.spans_dropped_api_error, 1);
            assert_eq!(report.bytes_sent, 0);

            ddog_trace_exporter_free(exporter);
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn stats_drain_and_shutdown_write_their_outputs_for_a_live_exporter() {
        unsafe {
            let server = MockServer::start();
            let exporter = observed_exporter(&server.base_url());

            let mut stats = MaybeUninit::<TraceExporterStatsObservations>::uninit();
            let error = ddog_trace_exporter_take_stats_observations(
                Some(exporter.as_ref()),
                NonNull::new(stats.as_mut_ptr()),
            );
            assert!(error.is_none());
            assert_eq!(stats.assume_init().collapsed_spans, [0; 16]);

            let mut final_stats = MaybeUninit::<TraceExporterStatsObservations>::uninit();
            let error = ddog_trace_exporter_shutdown_observed(
                Some(exporter),
                NonNull::new(final_stats.as_mut_ptr()),
            );
            assert!(error.is_none());
            assert_eq!(final_stats.assume_init().collapsed_spans, [0; 16]);

            // A null output discards the final drain but still shuts the exporter down.
            let exporter = observed_exporter(&server.base_url());
            assert!(ddog_trace_exporter_shutdown_observed(Some(exporter), None).is_none());
        }
    }

    #[test]
    fn shutdown_observed_rejects_a_missing_exporter_and_initialises_its_output() {
        unsafe {
            let mut stats = MaybeUninit::<TraceExporterStatsObservations>::uninit();
            let error =
                ddog_trace_exporter_shutdown_observed(None, NonNull::new(stats.as_mut_ptr()));
            assert_eq!(error.unwrap().code, ErrorCode::InvalidArgument);
            assert_eq!(stats.assume_init().collapsed_spans, [0; 16]);
        }
    }

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
