// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Client-side stats computation functionality for the trace exporter.
//!
//! This module handles the lifecycle and management of client-side stats computation,
//! including starting/stopping stats workers, managing the span concentrator,
//! and processing traces for stats collection.

use crate::agent_info::schema::AgentInfo;
use crate::stats_exporter;
use arc_swap::ArcSwap;
use libdd_common::runtime::Runtime;
use libdd_common::{Endpoint, MutexExt};
use libdd_trace_stats::span_concentrator::SpanConcentrator;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

use super::add_path;

pub(crate) const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] =
    ["client", "server", "producer", "consumer"];
pub(crate) const STATS_ENDPOINT: &str = "/v0.6/stats";

/// Context struct that groups immutable parameters used by stats functions
pub(crate) struct StatsContext<'a, R: Runtime> {
    pub metadata: &'a super::TracerMetadata,
    pub endpoint_url: &'a http::Uri,
    pub runtime: &'a Arc<Mutex<Option<Arc<R>>>>,
}

#[derive(Debug)]
pub(crate) enum StatsComputationStatus {
    /// Client-side stats has been disabled by the tracer
    Disabled,
    /// Client-side stats has been disabled by the agent or is not supported. It can be enabled
    /// later if the agent configuration changes. This is also the state used when waiting for the
    /// /info response.
    DisabledByAgent { bucket_size: Duration },
    /// Client-side stats is enabled
    Enabled {
        stats_concentrator: Arc<Mutex<SpanConcentrator>>,
        cancellation_token: CancellationToken,
    },
}

/// Get span kinds for stats computation with default fallback
fn get_span_kinds_for_stats(agent_info: &Arc<AgentInfo>) -> Vec<String> {
    agent_info
        .info
        .span_kinds_stats_computed
        .clone()
        .unwrap_or_else(|| DEFAULT_STATS_ELIGIBLE_SPAN_KINDS.map(String::from).to_vec())
}

/// Start the stats exporter and enable stats computation
///
/// Should only be used if the agent enabled stats computation
pub(crate) fn start_stats_computation<R: Runtime>(
    ctx: &StatsContext<R>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
    span_kinds: Vec<String>,
    peer_tags: Vec<String>,
    client: R::HttpClient,
) -> anyhow::Result<()> {
    if let StatsComputationStatus::DisabledByAgent { bucket_size } = **client_side_stats.load() {
        let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
            bucket_size,
            std::time::SystemTime::now(),
            span_kinds,
            peer_tags,
        )));
        let cancellation_token = CancellationToken::new();
        create_and_start_stats_worker(
            ctx,
            bucket_size,
            &stats_concentrator,
            &cancellation_token,
            workers,
            client_side_stats,
            client,
        )?;
    }
    Ok(())
}

/// Create stats exporter and worker, start the worker, and update the state
fn create_and_start_stats_worker<R: Runtime>(
    ctx: &StatsContext<R>,
    bucket_size: Duration,
    stats_concentrator: &Arc<Mutex<SpanConcentrator>>,
    cancellation_token: &CancellationToken,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client: R::HttpClient,
) -> anyhow::Result<()> {
    let stats_exporter = stats_exporter::StatsExporter::new(
        bucket_size,
        stats_concentrator.clone(),
        ctx.metadata.clone(),
        Endpoint::from_url(add_path(ctx.endpoint_url, STATS_ENDPOINT)),
        cancellation_token.clone(),
        client,
    );
    let mut stats_worker = crate::pausable_worker::PausableWorker::new(stats_exporter);

    // Get runtime guard
    let runtime_guard = ctx.runtime.lock_or_panic();
    if let Some(rt) = runtime_guard.as_ref() {
        stats_worker.start(rt.as_ref()).map_err(|e| {
            super::error::TraceExporterError::Internal(
                super::error::InternalErrorKind::InvalidWorkerState(e.to_string()),
            )
        })?;
    } else {
        return Err(anyhow::anyhow!("Runtime not available"));
    }

    // Update the stats computation state with the new worker and components
    workers.lock_or_panic().stats = Some(stats_worker);
    client_side_stats.store(Arc::new(StatsComputationStatus::Enabled {
        stats_concentrator: stats_concentrator.clone(),
        cancellation_token: cancellation_token.clone(),
    }));

    Ok(())
}

/// Stops the stats exporter and disable stats computation
///
/// Used when client-side stats is disabled by the agent
/// Generic version that doesn't use block_on (not implemented)
pub(crate) fn stop_stats_computation<R: Runtime>(
    ctx: &StatsContext<R>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
) {
    if let StatsComputationStatus::Enabled {
        stats_concentrator,
        cancellation_token,
    } = &**client_side_stats.load()
    {
        // If there's no runtime there's no exporter to stop
        let runtime_guard = ctx.runtime.lock_or_panic();
        if let Some(_rt) = runtime_guard.as_ref() {
            cancellation_token.cancel();
            workers.lock_or_panic().stats = None;
            let bucket_size = stats_concentrator.lock_or_panic().get_bucket_size();

            client_side_stats.store(Arc::new(StatsComputationStatus::DisabledByAgent {
                bucket_size,
            }));
        }
    }
}

/// Handle stats computation when agent changes from disabled to enabled
pub(crate) fn handle_stats_disabled_by_agent<R: Runtime>(
    ctx: &StatsContext<R>,
    agent_info: &Arc<AgentInfo>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
    client: R::HttpClient,
) {
    if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
        // Client-side stats is supported by the agent
        let status = start_stats_computation(
            ctx,
            client_side_stats,
            workers,
            get_span_kinds_for_stats(agent_info),
            agent_info.info.peer_tags.clone().unwrap_or_default(),
            client,
        );
        match status {
            Ok(()) => debug!("Client-side stats enabled"),
            Err(_) => error!("Failed to start stats computation"),
        }
    } else {
        debug!("Client-side stats computation has been disabled by the agent")
    }
}

/// Handle stats computation when it's already enabled (generic - not implemented)
pub(crate) fn handle_stats_enabled<R: Runtime>(
    ctx: &StatsContext<R>,
    agent_info: &Arc<AgentInfo>,
    stats_concentrator: &Mutex<SpanConcentrator>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
) {
    if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
        let mut concentrator = stats_concentrator.lock_or_panic();
        concentrator.set_span_kinds(get_span_kinds_for_stats(agent_info));
        concentrator.set_peer_tags(agent_info.info.peer_tags.clone().unwrap_or_default());
    } else {
        stop_stats_computation(ctx, client_side_stats, workers);
        debug!("Client-side stats computation has been disabled by the agent")
    }
}

/// Add all spans from the given iterator into the stats concentrator
/// # Panic
/// Will panic if another thread panicked will holding the lock on `stats_concentrator`
fn add_spans_to_stats<T: libdd_trace_utils::span::SpanText>(
    stats_concentrator: &Mutex<SpanConcentrator>,
    traces: &[Vec<libdd_trace_utils::span::Span<T>>],
) {
    let mut stats_concentrator = stats_concentrator.lock_or_panic();

    let spans = traces.iter().flat_map(|trace| trace.iter());
    for span in spans {
        stats_concentrator.add_span(span);
    }
}

/// Process traces for stats computation and update header tags accordingly
pub(crate) fn process_traces_for_stats<T: libdd_trace_utils::span::SpanText>(
    traces: &mut Vec<Vec<libdd_trace_utils::span::Span<T>>>,
    header_tags: &mut libdd_trace_utils::trace_utils::TracerHeaderTags,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client_computed_top_level: bool,
) {
    if let StatsComputationStatus::Enabled {
        stats_concentrator, ..
    } = &**client_side_stats.load()
    {
        if !client_computed_top_level {
            for chunk in traces.iter_mut() {
                libdd_trace_utils::span::trace_utils::compute_top_level_span(chunk);
            }
        }
        add_spans_to_stats(stats_concentrator, traces);
        // Once stats have been computed we can drop all chunks that are not going to be
        // sampled by the agent
        let libdd_trace_utils::span::trace_utils::DroppedP0Stats {
            dropped_p0_traces,
            dropped_p0_spans,
        } = libdd_trace_utils::span::trace_utils::drop_chunks(traces);

        // Update the headers to indicate that stats have been computed and forward dropped
        // traces counts
        header_tags.client_computed_top_level = true;
        header_tags.client_computed_stats = true;
        header_tags.dropped_p0_traces = dropped_p0_traces;
        header_tags.dropped_p0_spans = dropped_p0_spans;
    }
}

#[cfg(test)]
/// Test only function to check if the stats computation is active and the worker is running
pub(crate) fn is_stats_worker_active<R: Runtime>(
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    workers: &Arc<Mutex<super::TraceExporterWorkers<R>>>,
) -> bool {
    if !matches!(
        **client_side_stats.load(),
        StatsComputationStatus::Enabled { .. }
    ) {
        return false;
    }

    if let Ok(workers) = workers.try_lock() {
        if let Some(stats_worker) = &workers.stats {
            return matches!(
                stats_worker,
                crate::pausable_worker::PausableWorker::Running { .. }
            );
        }
    }

    false
}
