// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Periodic stats flusher for the SHM span concentrator.
//!
//! The sidecar maintains one `SpanConcentratorState` per (env, version, service) triple
//! (globally, across all sessions) in `SidecarServer::span_concentrators`.
//! Concentrators are created lazily on the first IPC span for a given key, and removed
//! automatically once idle: an empty drain sets the `please_reload` bit (telling PHP workers
//! to stop writing), and the subsequent flush performs a final drain before removal.

use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::shm_stats::{
    ShmSpanConcentrator, DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, RELOAD_FILL_RATIO,
};
use http::uri::PathAndQuery;
use libdd_capabilities_impl::{HttpClientTrait, NativeCapabilities};
use libdd_common::{Endpoint, MutexExt};
use libdd_trace_stats::stats_exporter::{StatsExporter, StatsMetadata};
use std::collections::HashMap;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tracing::{error, info, warn};
use zwohash::ZwoHasher;

/// Build the stats endpoint by appending `/v0.6/stats` to the agent base URL.
/// Returns `None` for agentless mode (API key present) — stats are not supported agentless.
pub(crate) fn stats_endpoint(endpoint: &Endpoint) -> Option<Endpoint> {
    if endpoint.api_key.is_some() {
        return None;
    }
    let mut parts = endpoint.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(
        libdd_trace_stats::stats_exporter::STATS_ENDPOINT_PATH,
    ));
    Some(Endpoint {
        url: http::Uri::from_parts(parts).ok()?,
        ..endpoint.clone()
    })
}

/// The subset of session configuration needed to create and flush a span stats concentrator.
#[derive(Clone)]
pub(crate) struct StatsConfig {
    /// Stats endpoint with final path already baked in.
    pub endpoint: Endpoint,
    pub flush_interval: Duration,
    /// Machine hostname, forwarded to the stats payload `hostname` field.
    pub hostname: String,
    /// Process-level tags serialised as `"key:value,..."`.
    pub process_tags: String,
    /// Process-level service name (from `DD_SERVICE`), used as the concentrator key dimension.
    pub root_service: String,
    /// Language identifier (e.g. "php").
    pub language: String,
    /// Tracer library version.
    pub tracer_version: String,
}

/// Map key for the per-(env, version, root-service) concentrator map.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct ConcentratorKey {
    pub env: String,
    pub version: String,
    pub root_service: String,
}

/// State held per-(env, version, root-service) for SHM span stats.
pub struct SpanConcentratorState {
    pub concentrator: ShmSpanConcentrator,
    /// The stats endpoint (with `/v0.6/stats` path baked in) used by the flush loop.
    pub(crate) endpoint: Endpoint,
    /// Metadata for StatsExporter payload annotation (hostname, env, version, service, …).
    pub(crate) meta: StatsMetadata,
}

// SAFETY: ShmSpanConcentrator is designed for cross-process sharing; all internal state
// uses atomic operations.
unsafe impl Send for SpanConcentratorState {}

/// Compute the SHM path for an (env, version, root-service) triple's span concentrator.
pub fn env_stats_shm_path(env: &str, version: &str, service: &str) -> CString {
    let mut hasher = ZwoHasher::default();
    env.hash(&mut hasher);
    version.hash(&mut hasher);
    service.hash(&mut hasher);
    let hash = hasher.finish();

    let mut path = format!(
        "/ddspsc{}-{}",
        crate::primary_sidecar_identifier(),
        BASE64_URL_SAFE_NO_PAD.encode(hash.to_ne_bytes()),
    );
    path.truncate(31);
    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

fn make_exporter(
    s: &SpanConcentratorState,
    endpoint: Endpoint,
    flush_interval: Duration,
) -> StatsExporter<NativeCapabilities, ShmSpanConcentrator> {
    StatsExporter::new(
        flush_interval,
        Arc::new(Mutex::new(s.concentrator.clone())),
        s.meta.clone(),
        endpoint,
        NativeCapabilities::new_client(),
    )
}

/// Spawn-and-forget flush loop for a concentrator.
///
/// **Idle removal**: when a flush produces no data (`send` returns `false`), the
/// `please_reload` bit is set on the SHM, signalling PHP workers to stop writing (they will
/// fall back to the IPC path).  On the very next tick, a final force-flush drains any
/// remaining data and the concentrator is removed from the map.  This two-phase removal
/// avoids a race between the reload signal and in-flight SHM writes.
pub async fn run_stats_flush_loop(
    states: Weak<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
    map_key: ConcentratorKey,
    flush_interval: Duration,
) {
    let Some(arc) = states.upgrade() else {
        return;
    };
    let state = {
        let guard = arc.lock_or_panic();
        guard.get(&map_key).cloned()
    };
    let Some(state) = state else {
        return;
    };
    let exporter = make_exporter(&state, state.endpoint.clone(), flush_interval);

    loop {
        tokio::time::sleep(flush_interval).await;
        let Some(arc) = states.upgrade() else {
            break; // sidecar shutting down
        };

        // Fill-check (atomic SHM reads, no lock needed).
        let (used, total) = state.concentrator.slot_usage();
        if total > 0 && (used as f64 / total as f64) > RELOAD_FILL_RATIO {
            warn!(
                "SHM span concentrator for env={} version={} service={} is {:.0}% full \
                 ({used}/{total} slots); consider increasing slot count",
                map_key.env,
                map_key.version,
                map_key.root_service,
                (used as f64 / total as f64) * 100.0
            );
        }

        match exporter.send(false).await {
            Err(e) => warn!(
                "Failed to send stats for env={} version={}: {e}",
                map_key.env, map_key.version
            ),
            Ok(true) => {} // data sent — continue
            Ok(false) => {
                // Empty drain: retire this concentrator.
                info!(
                    "Removing idle SHM span concentrator for env={} version={} service={}",
                    map_key.env, map_key.version, map_key.root_service,
                );
                state.concentrator.signal_reload();
                #[cfg(unix)]
                state.concentrator.unlink();
                #[cfg(unix)] // on windows waiting is pointless, because we cannot unlink it
                tokio::time::sleep(Duration::from_secs(1)).await;
                {
                    let mut guard = arc.lock_or_panic();
                    // Only remove our entry — a fresher one may have been inserted already.
                    if guard
                        .get(&map_key)
                        .map_or(false, |s| Arc::ptr_eq(s, &state))
                    {
                        guard.remove(&map_key);
                    }
                }
                if let Err(e) = exporter.send(true).await {
                    warn!("Failed final stats flush: {e}");
                }
                break;
            }
        }
    }
}

/// Look up or create the SHM span concentrator for `(env, version, service)`.
///
/// Called lazily from `add_span_to_concentrator` when the PHP worker could not write to SHM
/// directly (SHM not yet available).  Creating on first IPC span — rather than eagerly in
/// `set_universal_service_tags` — lets the concentrator key track the actual span env/version
/// rather than the root-span-only values reported at request start.
///
/// Returns `None` when stats config is not available (agentless or not yet configured).
pub(crate) fn get_or_create_concentrator(
    concentrators: &Arc<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
    env: &str,
    version: &str,
    runtime_id: &str,
    session: &crate::service::session_info::SessionInfo,
) -> Option<Arc<SpanConcentratorState>> {
    let config = session
        .stats_config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;

    if config.endpoint.api_key.is_some() {
        return None; // agentless — no stats
    }

    let service_name = config.root_service.clone();

    let map_key = ConcentratorKey {
        env: env.to_owned(),
        version: version.to_owned(),
        root_service: service_name.clone(),
    };
    let mut guard = concentrators.lock_or_panic();

    if let Some(s) = guard.get(&map_key) {
        if !s.concentrator.needs_reload() {
            return Some(s.clone());
        }
        // Entry is being retired (reload signalled) — fall through to create a fresh one.
    }

    let path = env_stats_shm_path(env, version, &service_name);

    let meta = StatsMetadata {
        hostname: config.hostname.clone(),
        env: env.to_owned(),
        app_version: version.to_owned(),
        runtime_id: runtime_id.to_owned(),
        language: config.language.clone(),
        tracer_version: config.tracer_version.clone(),
        process_tags: config.process_tags.clone(),
        service: service_name.clone(),
        ..Default::default()
    };

    match ShmSpanConcentrator::create(
        path.clone(),
        10_000_000_000,
        DEFAULT_SLOT_COUNT,
        DEFAULT_STRING_POOL_BYTES,
    ) {
        Ok(concentrator) => {
            let state = Arc::new(SpanConcentratorState {
                concentrator,
                endpoint: config.endpoint.clone(),
                meta,
            });
            guard.insert(map_key.clone(), state.clone());
            let weak = Arc::downgrade(concentrators);
            let flush_interval = config.flush_interval;
            tokio::spawn(async move {
                run_stats_flush_loop(weak, map_key, flush_interval).await;
            });
            Some(state)
        }
        Err(e) => {
            error!("Failed to create SHM span stats concentrator for env={env} version={version} service={service_name}: {e}");
            None
        }
    }
}

/// Immediately flush all active SHM span concentrators and send the results to the agent.
pub async fn flush_all_stats_now(
    state: &Arc<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
) {
    let states: Vec<Arc<SpanConcentratorState>> = {
        let guard = state.lock_or_panic();
        guard.values().cloned().collect()
    };
    for s in states {
        let exporter = make_exporter(&s, s.endpoint.clone(), Duration::from_secs(10));
        if let Err(e) = exporter.send(false).await {
            warn!("flush_all_stats_now: failed to send stats: {e}");
        }
    }
}
