// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Client-side stats computation functionality for the trace exporter.
//!
//! This module handles the lifecycle and management of client-side stats computation,
//! including starting/stopping stats workers, managing the span concentrator,
//! and processing traces for stats collection.

#[cfg(not(target_arch = "wasm32"))]
use crate::agent_info::schema::AgentInfo;
use arc_swap::ArcSwap;
use libdd_capabilities::{HttpClientTrait, MaybeSend};
#[cfg(not(target_arch = "wasm32"))]
use libdd_common::Endpoint;
use libdd_common::MutexExt;
use libdd_shared_runtime::{SharedRuntime, WorkerHandle};
use libdd_trace_stats::span_concentrator::SpanConcentrator;
use libdd_trace_stats::stats_exporter::{StatsExporter, StatsMetadata};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use tracing::{debug, error};

#[cfg(not(target_arch = "wasm32"))]
use super::add_path;
use super::TracerMetadata;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] =
    ["client", "server", "producer", "consumer"];
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const STATS_ENDPOINT: &str = "/v0.6/stats";

#[cfg(not(target_arch = "wasm32"))]
/// Context struct that groups immutable parameters used by stats functions
pub(crate) struct StatsContext<'a> {
    pub metadata: &'a TracerMetadata,
    pub endpoint_url: &'a http::Uri,
    pub shared_runtime: &'a SharedRuntime,
}

#[derive(Debug)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
        worker_handle: WorkerHandle,
    },
}

#[cfg(not(target_arch = "wasm32"))]
/// Get span kinds for stats computation with default fallback
fn get_span_kinds_for_stats(agent_info: &Arc<AgentInfo>) -> Vec<String> {
    agent_info
        .info
        .span_kinds_stats_computed
        .clone()
        .unwrap_or_else(|| DEFAULT_STATS_ELIGIBLE_SPAN_KINDS.map(String::from).to_vec())
}

#[cfg(not(target_arch = "wasm32"))]
/// Start the stats exporter and enable stats computation
///
/// Should only be used if the agent enabled stats computation
pub(crate) fn start_stats_computation<H: HttpClientTrait + MaybeSend + Sync + 'static>(
    ctx: &StatsContext,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    span_kinds: Vec<String>,
    peer_tags: Vec<String>,
    client: H,
) -> anyhow::Result<()> {
    if let StatsComputationStatus::DisabledByAgent { bucket_size } = **client_side_stats.load() {
        let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
            bucket_size,
            std::time::SystemTime::now(),
            span_kinds,
            peer_tags,
        )));
        create_and_start_stats_worker(
            ctx,
            bucket_size,
            &stats_concentrator,
            client_side_stats,
            client,
        )?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Create stats exporter and worker, start the worker, and update the state
fn create_and_start_stats_worker<H: HttpClientTrait + MaybeSend + Sync + 'static>(
    ctx: &StatsContext,
    bucket_size: Duration,
    stats_concentrator: &Arc<Mutex<SpanConcentrator>>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client: H,
) -> anyhow::Result<()> {
    let stats_exporter = StatsExporter::<H>::new(
        bucket_size,
        stats_concentrator.clone(),
        StatsMetadata::from(ctx.metadata.clone()),
        Endpoint::from_url(add_path(ctx.endpoint_url, STATS_ENDPOINT)),
        client,
    );
    let worker_handle = ctx
        .shared_runtime
        .spawn_worker(stats_exporter)
        .map_err(|e| anyhow::anyhow!(e))?;

    // Update the stats computation state with the new worker components.
    client_side_stats.store(Arc::new(StatsComputationStatus::Enabled {
        stats_concentrator: stats_concentrator.clone(),
        worker_handle,
    }));

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Stops the stats exporter and disable stats computation
///
/// Used when client-side stats is disabled by the agent
pub(crate) fn stop_stats_computation(
    ctx: &StatsContext,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
) {
    if let StatsComputationStatus::Enabled {
        stats_concentrator,
        worker_handle,
    } = &**client_side_stats.load()
    {
        let bucket_size = stats_concentrator.lock_or_panic().get_bucket_size();
        client_side_stats.store(Arc::new(StatsComputationStatus::DisabledByAgent {
            bucket_size,
        }));
        match ctx.shared_runtime.block_on(worker_handle.clone().stop()) {
            Ok(Err(e)) => error!("Failed to stop stats worker: {e}"),
            Err(e) => error!("Failed to stop stats worker: {e}"),
            _ => {}
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Handle stats computation when agent changes from disabled to enabled
pub(crate) fn handle_stats_disabled_by_agent<H: HttpClientTrait + MaybeSend + Sync + 'static>(
    ctx: &StatsContext,
    agent_info: &Arc<AgentInfo>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client: H,
) {
    if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
        let status = start_stats_computation(
            ctx,
            client_side_stats,
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

#[cfg(not(target_arch = "wasm32"))]
/// Handle stats computation when it's already enabled
pub(crate) fn handle_stats_enabled(
    ctx: &StatsContext,
    agent_info: &Arc<AgentInfo>,
    stats_concentrator: &Mutex<SpanConcentrator>,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
) {
    if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
        let mut concentrator = stats_concentrator.lock_or_panic();
        concentrator.set_span_kinds(get_span_kinds_for_stats(agent_info));
        concentrator.set_peer_tags(agent_info.info.peer_tags.clone().unwrap_or_default());
    } else {
        stop_stats_computation(ctx, client_side_stats);
        debug!("Client-side stats computation has been disabled by the agent")
    }
}

/// Add all spans from the given iterator into the stats concentrator
/// # Panic
/// Will panic if another thread panicked will holding the lock on `stats_concentrator`
fn add_spans_to_stats<T: libdd_trace_utils::span::TraceData>(
    stats_concentrator: &Mutex<SpanConcentrator>,
    traces: &[Vec<libdd_trace_utils::span::v04::Span<T>>],
) {
    let mut stats_concentrator = stats_concentrator.lock_or_panic();

    let spans = traces.iter().flat_map(|trace| trace.iter());
    for span in spans {
        stats_concentrator.add_span(span);
    }
}

/// Process traces for stats computation and update header tags accordingly.
/// Returns the number of P0 traces and spans that were dropped.
pub(crate) fn process_traces_for_stats<T: libdd_trace_utils::span::TraceData>(
    traces: &mut Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    header_tags: &mut libdd_trace_utils::trace_utils::TracerHeaderTags,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client_computed_top_level: bool,
) -> libdd_trace_utils::span::trace_utils::DroppedP0Stats {
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
        let dropped_p0_stats = libdd_trace_utils::span::trace_utils::drop_chunks(traces);

        // Update the headers to indicate that stats have been computed and forward dropped
        // traces counts
        header_tags.client_computed_top_level = true;
        header_tags.client_computed_stats = true;
        header_tags.dropped_p0_traces = dropped_p0_stats.dropped_p0_traces;
        header_tags.dropped_p0_spans = dropped_p0_stats.dropped_p0_spans;

        dropped_p0_stats
    } else {
        libdd_trace_utils::span::trace_utils::DroppedP0Stats {
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
/// Test only function to check if the stats computation is active and the worker is running
pub(crate) fn is_stats_worker_active(client_side_stats: &ArcSwap<StatsComputationStatus>) -> bool {
    matches!(
        **client_side_stats.load(),
        StatsComputationStatus::Enabled { .. }
    )
}

impl From<TracerMetadata> for StatsMetadata {
    fn from(m: TracerMetadata) -> StatsMetadata {
        StatsMetadata {
            hostname: m.hostname,
            env: m.env,
            app_version: m.app_version,
            runtime_id: m.runtime_id,
            language: m.language,
            lang_version: m.language_version,
            lang_interpreter: m.language_interpreter,
            lang_vendor: m.language_interpreter_vendor,
            tracer_version: m.tracer_version,
            git_commit_sha: m.git_commit_sha,
            process_tags: m.process_tags,
            service: m.service,
        }
    }
}
