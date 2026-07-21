// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Client-side stats computation functionality for the trace exporter.
//!
//! This module handles the lifecycle and management of client-side stats computation,
//! including starting/stopping stats workers, managing the span concentrator,
//! and processing traces for stats collection.

pub use libdd_trace_stats::span_concentrator::CardinalityLimitConfig;

use super::add_path;
use super::TracerMetadata;
use crate::agent_info::schema::AgentInfo;
use arc_swap::ArcSwap;
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_common::Endpoint;
use libdd_common::MutexExt;
use libdd_shared_runtime::{SharedRuntime, WorkerHandle};
use libdd_trace_stats::span_concentrator::SpanConcentrator;
#[cfg(feature = "stats-obfuscation")]
use libdd_trace_stats::span_concentrator::{
    SharedStatsComputationObfuscationConfig, StatsComputationObfuscationConfig,
};
use libdd_trace_stats::stats_exporter::{StatsExporter, StatsMetadata};
use libdd_trace_utils::trace_filter::TraceFilterer;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error};
// std::time::SystemTime::now() panics on wasm32.
use web_time::SystemTime;

pub(crate) const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] =
    ["client", "server", "producer", "consumer"];
pub(crate) const STATS_ENDPOINT: &str = "/v0.6/stats";

/// The maximum obfuscation version this tracer supports.
#[cfg(feature = "stats-obfuscation")]
pub(crate) const SUPPORTED_OBFUSCATION_VERSION: u32 = 1;
#[cfg(feature = "stats-obfuscation")]
pub(crate) const SUPPORTED_OBFUSCATION_VERSION_STR: &str = "1";

/// Context struct that groups immutable parameters used by stats functions
pub(crate) struct StatsContext<
    'a,
    C: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> {
    pub metadata: &'a TracerMetadata,
    pub endpoint_url: &'a http::Uri,
    pub shared_runtime: &'a R,
    pub stats_cardinality_limits: Option<CardinalityLimitConfig>,
    /// Optional DogStatsD client forwarded to the [`StatsExporter`].
    pub dogstatsd: Option<std::sync::Arc<libdd_dogstatsd_client::Client>>,
    /// Optional telemetry handle forwarded to the [`StatsExporter`].
    #[cfg(feature = "telemetry")]
    pub telemetry: Option<libdd_telemetry::worker::TelemetryWorkerHandle<C>>,
    #[cfg(not(feature = "telemetry"))]
    pub(crate) _phantom: std::marker::PhantomData<fn() -> C>,
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
        worker_handle: WorkerHandle,
    },
}

#[derive(Debug)]
pub(crate) struct StatsComputationConfig {
    pub(crate) status: ArcSwap<StatsComputationStatus>,
    pub(crate) stats_cardinality_limits: Option<CardinalityLimitConfig>,
    #[cfg(feature = "stats-obfuscation")]
    pub(crate) obfuscation_config: SharedStatsComputationObfuscationConfig,
    /// Builder-level opt-in. When false, stats obfuscation stays off
    /// regardless of agent support.
    #[cfg(feature = "stats-obfuscation")]
    pub(crate) obfuscation_enabled: bool,
}

/// Return true if the agent supports client-side stats.
///
/// This requires:
/// - `client_drop_p0s` to be enabled on the agent,
/// - the `/v0.6/stats` endpoint to be advertised by the agent.
fn is_stats_computation_supported(agent_info: &AgentInfo) -> bool {
    agent_info.info.client_drop_p0s.is_some_and(|v| v)
        && agent_info
            .info
            .endpoints
            .as_ref()
            .is_some_and(|endpoints| endpoints.iter().any(|e| e == STATS_ENDPOINT))
}

/// Return true if the agent's obfuscation version is supported by this tracer
#[cfg(feature = "stats-obfuscation")]
fn is_obfuscation_active(agent_info: &AgentInfo) -> bool {
    agent_info
        .info
        .obfuscation_version
        .is_some_and(|v| v >= 1 && v == SUPPORTED_OBFUSCATION_VERSION)
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
pub(crate) fn start_stats_computation<
    C: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
>(
    ctx: &StatsContext<C, R>,
    span_kinds: Vec<String>,
    peer_tags: Vec<String>,
    capabilities: C,
    client_side_stats: &StatsComputationConfig,
) -> anyhow::Result<()> {
    if let StatsComputationStatus::DisabledByAgent { bucket_size } =
        **client_side_stats.status.load()
    {
        let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
            bucket_size,
            SystemTime::now(),
            span_kinds,
            peer_tags,
            ctx.stats_cardinality_limits,
            #[cfg(feature = "stats-obfuscation")]
            Some(client_side_stats.obfuscation_config.clone()),
        )));
        create_and_start_stats_worker(ctx, &stats_concentrator, capabilities, client_side_stats)?;
    }
    Ok(())
}

/// Create stats exporter and worker, start the worker, and update the state
fn create_and_start_stats_worker<
    C: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
>(
    ctx: &StatsContext<C, R>,
    stats_concentrator: &Arc<Mutex<SpanConcentrator>>,
    capabilities: C,
    client_side_stats: &StatsComputationConfig,
) -> anyhow::Result<()> {
    let bucket_size = stats_concentrator.lock_or_panic().get_bucket_size();
    let stats_exporter = StatsExporter::<C>::new(
        bucket_size,
        stats_concentrator.clone(),
        StatsMetadata::from(ctx.metadata.clone()),
        Endpoint::from_url(add_path(ctx.endpoint_url, STATS_ENDPOINT)),
        capabilities.clone(),
        #[cfg(feature = "stats-obfuscation")]
        SUPPORTED_OBFUSCATION_VERSION_STR,
        #[cfg(feature = "telemetry")]
        ctx.telemetry.clone(),
        ctx.dogstatsd.clone(),
    );
    let worker_handle = ctx
        .shared_runtime
        .spawn_worker(stats_exporter, false)
        .map_err(|e| anyhow::anyhow!(e))?;

    // Update the stats computation state with the new worker components.
    client_side_stats
        .status
        .store(Arc::new(StatsComputationStatus::Enabled {
            stats_concentrator: stats_concentrator.clone(),
            worker_handle,
        }));

    Ok(())
}

/// Transition from `Enabled` to `DisabledByAgent`, awaiting the stats worker shutdown.
pub(crate) async fn stop_stats_computation(client_side_stats: &ArcSwap<StatsComputationStatus>) {
    // load_full() avoids holding an ArcSwap Guard (!Send) across .await.
    let snapshot = client_side_stats.load_full();
    if let StatsComputationStatus::Enabled {
        stats_concentrator,
        worker_handle,
        ..
    } = &*snapshot
    {
        let bucket_size = stats_concentrator.lock_or_panic().get_bucket_size();
        client_side_stats.store(Arc::new(StatsComputationStatus::DisabledByAgent {
            bucket_size,
        }));
        if let Err(e) = worker_handle.clone().stop().await {
            error!("Failed to stop stats worker: {e}");
        }
    }
}

/// Handle stats computation when agent changes from disabled to enabled
pub(crate) fn handle_stats_disabled_by_agent<
    C: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
>(
    ctx: &StatsContext<C, R>,
    agent_info: &Arc<AgentInfo>,
    capabilities: C,
    client_side_stats: &StatsComputationConfig,
) {
    if is_stats_computation_supported(agent_info) {
        let status = start_stats_computation(
            ctx,
            get_span_kinds_for_stats(agent_info),
            agent_info.info.peer_tags.clone().unwrap_or_default(),
            capabilities,
            client_side_stats,
        );
        match status {
            Ok(()) => {
                #[cfg(feature = "stats-obfuscation")]
                update_obfuscation_config(agent_info, client_side_stats);
                debug!("Client-side stats enabled");
            }
            Err(_) => error!("Failed to start stats computation"),
        }
    } else {
        debug!("Client-side stats computation has been disabled by the agent")
    }
}

#[cfg(feature = "stats-obfuscation")]
fn update_obfuscation_config(
    agent_info: &Arc<AgentInfo>,
    client_side_stats: &StatsComputationConfig,
) {
    if matches!(
        &**client_side_stats.status.load(),
        StatsComputationStatus::Enabled { .. }
    ) {
        let obfuscation_active =
            client_side_stats.obfuscation_enabled && is_obfuscation_active(agent_info);
        // FIXME(APMSP-3720): there is more than this to obfuscation config
        let sql_obfuscation_mode = (|| {
            agent_info
                .info
                .config
                .as_ref()?
                .obfuscation
                .as_ref()?
                .sql_obfuscation_mode
        })()
        .unwrap_or_default();
        client_side_stats
            .obfuscation_config
            .store(Arc::new(StatsComputationObfuscationConfig {
                enabled: obfuscation_active,
                sql_obfuscation_mode,
            }));
    }
}

pub(crate) async fn handle_stats_enabled(
    agent_info: &Arc<AgentInfo>,
    stats_concentrator: &Arc<Mutex<SpanConcentrator>>,
    client_side_stats: &StatsComputationConfig,
) {
    if is_stats_computation_supported(agent_info) {
        let mut concentrator = stats_concentrator.lock_or_panic();
        concentrator.set_span_kinds(get_span_kinds_for_stats(agent_info));
        concentrator.set_peer_tags(agent_info.info.peer_tags.clone().unwrap_or_default());
        #[cfg(feature = "stats-obfuscation")]
        update_obfuscation_config(agent_info, client_side_stats);
    } else {
        stop_stats_computation(&client_side_stats.status).await;
        debug!("Client-side stats computation has been disabled by the agent")
    }
}

/// Add all spans from the given iterator into the stats concentrator, optionally obfuscating
/// resource names for client-side stats.
///
/// # Panic
/// Will panic if another thread panicked while holding the lock on `stats_concentrator`
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
///
/// If a telemetry client is provided and stats are enabled, dropped P0 counts
/// will be sent to telemetry.
pub(crate) fn process_traces_for_stats<
    T: libdd_trace_utils::span::TraceData,
    #[cfg(feature = "telemetry")] C: libdd_capabilities::HttpClientCapability
        + libdd_capabilities::SleepCapability
        + libdd_capabilities::MaybeSend
        + Sync
        + 'static,
>(
    traces: &mut Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    header_tags: &mut libdd_trace_utils::trace_utils::TracerHeaderTags,
    client_side_stats: &ArcSwap<StatsComputationStatus>,
    client_computed_top_level: bool,
    trace_filterer: &TraceFilterer,
    #[cfg(feature = "telemetry")] telemetry: Option<&crate::telemetry::TelemetryClient<C>>,
) {
    let status = client_side_stats.load();
    if let StatsComputationStatus::Enabled {
        stats_concentrator, ..
    } = &**status
    {
        let dropped_by_trace_filter = trace_filterer.filter_traces(traces);
        #[cfg(not(all(not(target_arch = "wasm32"), feature = "telemetry")))]
        let _ = dropped_by_trace_filter;

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

        // Send dropped P0 stats directly to telemetry if available
        #[cfg(feature = "telemetry")]
        if let Some(telemetry_client) = telemetry {
            if let Err(e) = telemetry_client.send_client_side_stats_drops(
                dropped_p0_stats.dropped_p0_traces,
                dropped_by_trace_filter,
            ) {
                tracing::error!(?e, "Error sending dropped P0 stats to telemetry");
            }
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn test_obfuscation_version_was_updated() {
        use crate::trace_exporter::stats::{
            SUPPORTED_OBFUSCATION_VERSION, SUPPORTED_OBFUSCATION_VERSION_STR,
        };

        assert_eq!(
            SUPPORTED_OBFUSCATION_VERSION.to_string(),
            SUPPORTED_OBFUSCATION_VERSION_STR
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod is_stats_computation_supported {
        use crate::agent_info::schema::{AgentInfo, AgentInfoStruct};
        use crate::trace_exporter::stats::{is_stats_computation_supported, STATS_ENDPOINT};

        fn make_agent_info(
            client_drop_p0s: Option<bool>,
            endpoints: Option<Vec<&str>>,
        ) -> AgentInfo {
            AgentInfo {
                state_hash: String::new(),
                info: AgentInfoStruct {
                    client_drop_p0s,
                    endpoints: endpoints.map(|e| e.into_iter().map(String::from).collect()),
                    ..Default::default()
                },
            }
        }

        #[test]
        fn supported_when_all_requirements_met() {
            let info = make_agent_info(Some(true), Some(vec!["/v0.4/traces", STATS_ENDPOINT]));
            assert!(is_stats_computation_supported(&info));
        }

        #[test]
        fn unsupported_when_client_drop_p0s_missing_or_false() {
            let info = make_agent_info(None, Some(vec![STATS_ENDPOINT]));
            assert!(!is_stats_computation_supported(&info));

            let info = make_agent_info(Some(false), Some(vec![STATS_ENDPOINT]));
            assert!(!is_stats_computation_supported(&info));
        }

        #[test]
        fn unsupported_when_stats_endpoint_absent() {
            let info = make_agent_info(Some(true), Some(vec!["/v0.4/traces"]));
            assert!(!is_stats_computation_supported(&info));

            let info = make_agent_info(Some(true), None);
            assert!(!is_stats_computation_supported(&info));
        }
    }
}
