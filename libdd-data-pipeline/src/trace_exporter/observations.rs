// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! External observations: exporter measurements returned to the caller instead of being
//! delivered by the native telemetry client.
//!
//! This whole module is the `external-observations` feature and is meant to be deleted as a
//! unit. It is a temporary bridge for hosts that run their own telemetry client; the only
//! other places that know about it are the hook calls in [`super::observation_hooks`], the
//! builder switch, and the FFI crate's `observations` module.
//!
//! A report is scoped to one observed call through a task-local, so the send path needs no
//! extra parameters and concurrent callers cannot consume each other's observations.

use super::{AgentResponse, TraceExporter, TraceExporterError};
use crate::telemetry::SendPayloadTelemetry;
use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::span::TraceData;
use libdd_trace_utils::span::span_pool::PooledChunks;
use std::future::Future;
use std::sync::{Arc, Mutex};

#[cfg(not(target_arch = "wasm32"))]
use libdd_shared_runtime::BlockingRuntime;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static REPORT: Arc<Mutex<SendObservations>>;
}

/// Observations attributable to one send. Cancellation may leave only filtering counts.
/// Each exporter send currently prepares at most one payload, including V1 fallback.
#[derive(Debug, Default)]
pub struct SendObservations {
    pub payload: Option<SendPayloadTelemetry>,
    pub chunks_dropped_p0: u64,
    pub chunks_dropped_by_trace_filter: u64,
}

fn with_report(update: impl FnOnce(&mut SendObservations)) -> bool {
    REPORT
        .try_with(|report| update(&mut report.lock().unwrap_or_else(|e| e.into_inner())))
        .is_ok()
}

/// Record a payload measurement in the current call's report. Returns whether a report is
/// active, in which case native telemetry must not also receive the measurement.
pub(super) fn record_payload(payload: impl FnOnce() -> SendPayloadTelemetry) -> bool {
    with_report(|report| report.payload = Some(payload()))
}

/// Record chunks dropped by client-side stats filtering in the current call's report.
/// Returns whether a report is active.
pub(super) fn record_dropped(p0_chunks: usize, trace_filter_chunks: usize) -> bool {
    with_report(|report| {
        report.chunks_dropped_p0 = u64::try_from(p0_chunks).unwrap_or(u64::MAX);
        report.chunks_dropped_by_trace_filter =
            u64::try_from(trace_filter_chunks).unwrap_or(u64::MAX);
    })
}

/// Run `fut` with a fresh report, returning it along with the output. Observations already
/// produced survive the future being dropped part-way, for instance on cancellation.
async fn observe<F: Future>(fut: F) -> (F::Output, SendObservations) {
    let report = Arc::new(Mutex::new(SendObservations::default()));
    let output = REPORT.scope(report.clone(), fut).await;
    let observations = std::mem::take(&mut *report.lock().unwrap_or_else(|e| e.into_inner()));
    (output, observations)
}

impl<
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> TraceExporter<C, R>
{
    /// Atomically consume background stats observations. Slot zero denotes whole-key
    /// collapse; other slots denote combinations of the four collapsed-field bits.
    /// Returns zeros when external observations were not enabled on the builder.
    pub fn take_stats_observations(&self) -> [u64; 16] {
        self.observations.as_ref().map_or([0; 16], |o| o.take())
    }

    /// Stop workers, then consume their final stats deltas. A timed-out shutdown may
    /// discard observations from work that did not finish before the deadline.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn shutdown_observed(
        self,
        timeout: Option<Duration>,
    ) -> (Result<(), TraceExporterError>, [u64; 16])
    where
        R: BlockingRuntime,
    {
        let observations = self.observations.clone();
        let result = self.shutdown(timeout);
        (result, observations.map_or([0; 16], |o| o.take()))
    }

    /// Send msgpack with observations, including empty reports for decoding failures.
    pub async fn send_observed_async(
        &self,
        data: &[u8],
    ) -> (Result<AgentResponse, TraceExporterError>, SendObservations) {
        observe(self.send_async(data)).await
    }

    /// Blocking counterpart of [`Self::send_observed_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_observed(
        &self,
        data: &[u8],
    ) -> (Result<AgentResponse, TraceExporterError>, SendObservations)
    where
        R: BlockingRuntime,
    {
        self.shared_runtime
            .block_on(self.send_observed_async(data))
            .unwrap_or_else(|e| (Err(e.into()), SendObservations::default()))
    }

    /// Asynchronously send chunks with call-local observations and no native submission.
    pub async fn send_trace_chunks_observed_async<T: TraceData>(
        &self,
        trace_chunks: PooledChunks<'_, T>,
    ) -> (Result<AgentResponse, TraceExporterError>, SendObservations) {
        observe(self.send_trace_chunks_async(trace_chunks)).await
    }

    /// Send chunks and return measurements on both success and failure, without submitting
    /// this operation's measurements to the native telemetry client. The report is local to
    /// the call, so concurrent callers cannot consume each other's observations. Cancellation
    /// preserves observations already produced, but does not invent a terminal retry result.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_trace_chunks_observed<T: TraceData>(
        &self,
        trace_chunks: PooledChunks<'_, T>,
        cancellation_token: Option<&CancellationToken>,
    ) -> (Result<AgentResponse, TraceExporterError>, SendObservations)
    where
        R: BlockingRuntime,
    {
        self.shared_runtime
            .block_on(observe(
                self.send_trace_chunks_cancellable(trace_chunks, cancellation_token),
            ))
            .unwrap_or_else(|e| (Err(e.into()), SendObservations::default()))
    }
}
