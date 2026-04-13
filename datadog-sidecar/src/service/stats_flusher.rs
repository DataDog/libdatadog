// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Periodic stats flusher for the SHM span concentrator.
//!
//! The sidecar maintains one `SpanConcentratorState` per (env, version, service) triple
//! (globally, across all sessions) in `SidecarServer::span_concentrators`
//! (a `HashMap<ConcentratorKey, SpanConcentratorState>`).  A tokio task creates a
//! `StatsExporter` backed by the SHM concentrator and periodically calls `send`, which
//! drains the inactive bucket and POSTs it to the agent's `/v0.6/stats` endpoint.

use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::shm_stats::{
    ShmSpanConcentrator, DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, RELOAD_FILL_RATIO,
};
use http::uri::PathAndQuery;
use libdd_capabilities_impl::{HttpClientTrait, NativeCapabilities};
use libdd_common::Endpoint;
use libdd_trace_stats::stats_exporter::{StatsExporter, StatsMetadata};
use std::collections::HashMap;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use zwohash::ZwoHasher;

/// After the last `SpanConcentratorGuard` is dropped, keep the concentrator alive for this long
/// before removing it (to absorb late-arriving spans from the previous app version/env).
const IDLE_REMOVE_SECS: u64 = 600;

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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    pub path: CString,
    /// Number of live `SpanConcentratorGuard`s referring to this entry.
    pub(crate) ref_count: Arc<AtomicUsize>,
    /// Unix timestamp (seconds) when `ref_count` last dropped to zero; `u64::MAX` while active.
    pub(crate) last_zero_secs: Arc<AtomicU64>,
    /// The stats endpoint (with `/v0.6/stats` path baked in) used by the flush loop.
    pub(crate) endpoint: Endpoint,
    /// Metadata for StatsExporter payload annotation (hostname, env, version, service, …).
    pub(crate) meta: StatsMetadata,
}

// SAFETY: ShmSpanConcentrator is designed for cross-process sharing; all internal state
// uses atomic operations.  The Mutex in SessionInfo guards exclusive sidecar access.
unsafe impl Send for SpanConcentratorState {}

/// RAII guard that keeps an (env, version, root-service) concentrator alive.
///
/// Stored in `ActiveApplication`.  When the last guard for a given (env, version, root-service)
/// is dropped, the flush loop will remove the concentrator after `IDLE_REMOVE_SECS` seconds.
pub struct SpanConcentratorGuard {
    ref_count: Arc<AtomicUsize>,
    last_zero_secs: Arc<AtomicU64>,
}

impl Drop for SpanConcentratorGuard {
    fn drop(&mut self) {
        if self.ref_count.fetch_sub(1, Release) == 1 {
            // We were the last active reference — record when the idle period started.
            self.last_zero_secs.store(now_secs(), Release);
        }
    }
}

/// Compute the SHM path for an (env, version, root-service) triple's span concentrator.
///
/// Uses the same scheme as `agent_remote_config.rs` and `agent_info.rs`:
/// `/ddspsc-{uid}-{hash(env+version+service)}`, truncated to 31 chars (macOS limit).
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

/// Build a `StatsExporter` for a concentrator state.
///
/// The SHM concentrator is cloned (cheap — same underlying `Arc<MappedMem>`) and wrapped in
/// `Arc<Mutex<>>` as required by `StatsExporter`.  The mutex only guards the `flush_buckets`
/// `&mut self` requirement; the actual SHM operations remain lock-free.
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

/// Spawn-and-forget flush loop for an (env, version, root-service) pair's SHM span concentrator.
///
/// The loop exits when the `Weak` can no longer be upgraded (sidecar shutting down), when the
/// entry for this key is removed from the map, or when the concentrator has been idle (no active
/// `SpanConcentratorGuard`s) for `IDLE_REMOVE_SECS` seconds.
///
/// The endpoint (including test-session token) is read from `SpanConcentratorState` on every
/// tick so that late endpoint updates (e.g. a test-session token set after concentrator creation)
/// are picked up automatically.
pub async fn run_stats_flush_loop(
    states: Weak<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
    map_key: ConcentratorKey,
    flush_interval: Duration,
) {
    // Build the initial exporter.  The concentrator clone shares the same SHM mapping.
    let Some(arc) = states.upgrade() else {
        return;
    };
    let state = {
        let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
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

        let (state, force_and_clean) = {
            let mut guard = arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = guard.get(&map_key) else {
                break; // concentrator was removed externally
            };
            let idle_secs = if s.ref_count.load(Acquire) == 0 {
                let last_zero = s.last_zero_secs.load(Acquire);
                if last_zero != u64::MAX {
                    now_secs().saturating_sub(last_zero)
                } else {
                    0
                }
            } else {
                0
            };
            if idle_secs >= IDLE_REMOVE_SECS {
                info!(
                    "Removing idle SHM span concentrator for env={} version={} service={} \
                     (idle for {idle_secs}s)",
                    map_key.env, map_key.version, map_key.root_service,
                );
                #[allow(clippy::expect_used)]
                (
                    guard
                        .remove(&map_key)
                        .expect("removal after access in lock"),
                    true,
                )
            } else {
                (s.clone(), false)
            }
        };

        // Fill-check (atomic SHM read, no lock needed).
        let (used, total) = state.concentrator.slot_usage();
        if total > 0 {
            let fill = used as f64 / total as f64;
            if fill > RELOAD_FILL_RATIO {
                warn!(
                    "SHM span concentrator for env={} version={} service={} is {:.0}% full \
                     ({used}/{total} slots); consider increasing slot count",
                    map_key.env,
                    map_key.version,
                    map_key.root_service,
                    fill * 100.0
                );
            }
        }

        // Flush and send.  force=true on idle removal to drain both buckets.
        if let Err(e) = exporter.send(force_and_clean).await {
            warn!(
                "Failed to send stats for env={} version={}: {e}",
                map_key.env, map_key.version
            );
        }

        if force_and_clean {
            break;
        }
    }
}

/// Create (or look up) the SHM span concentrator for an (env, version, service) pair, increment
/// its reference count, and return a guard.
pub(crate) fn ensure_stats_concentrator(
    concentrators: &Arc<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
    env: &str,
    version: &str,
    service_name: &str,
    runtime_id: &str,
    session: &crate::service::session_info::SessionInfo,
) -> Option<SpanConcentratorGuard> {
    let config = session
        .stats_config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;

    // Stats computation requires a local agent; skip for agentless (API key present).
    if config.endpoint.api_key.is_some() {
        return None;
    }
    let endpoint = config.endpoint.clone();

    let map_key = ConcentratorKey {
        env: env.to_owned(),
        version: version.to_owned(),
        root_service: service_name.to_owned(),
    };
    let mut guard = concentrators.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(s) = guard.get_mut(&map_key) {
        // Concentrator already exists — increment ref count and reset idle timer.
        s.last_zero_secs.store(u64::MAX, Release);
        s.ref_count.fetch_add(1, AcqRel);
        return Some(SpanConcentratorGuard {
            ref_count: s.ref_count.clone(),
            last_zero_secs: s.last_zero_secs.clone(),
        });
    }

    let path = env_stats_shm_path(env, version, service_name);
    let bucket_nanos: u64 = 10_000_000_000; // 10 s

    let meta = StatsMetadata {
        hostname: config.hostname.clone(),
        env: env.to_owned(),
        app_version: version.to_owned(),
        runtime_id: runtime_id.to_owned(),
        language: config.language.clone(),
        tracer_version: config.tracer_version.clone(),
        process_tags: config.process_tags.clone(),
        service: service_name.to_owned(),
        ..Default::default()
    };

    match ShmSpanConcentrator::create(
        path.clone(),
        bucket_nanos,
        DEFAULT_SLOT_COUNT,
        DEFAULT_STRING_POOL_BYTES,
    ) {
        Ok(concentrator) => {
            let ref_count = Arc::new(AtomicUsize::new(1));
            let last_zero_secs = Arc::new(AtomicU64::new(u64::MAX));
            let app_guard = SpanConcentratorGuard {
                ref_count: ref_count.clone(),
                last_zero_secs: last_zero_secs.clone(),
            };
            guard.insert(
                map_key.clone(),
                Arc::new(SpanConcentratorState {
                    concentrator,
                    path,
                    ref_count,
                    last_zero_secs,
                    endpoint,
                    meta,
                }),
            );
            let weak = Arc::downgrade(concentrators);
            let flush_interval = config.flush_interval;
            tokio::spawn(async move {
                run_stats_flush_loop(weak, map_key, flush_interval).await;
            });
            Some(app_guard)
        }
        Err(e) => {
            error!(
                "Failed to create SHM span stats concentrator for env={env} version={version}: {e}"
            );
            None
        }
    }
}

/// Immediately flush all active SHM span concentrators and send the results to the agent.
///
/// Called by the sidecar's `flush_traces` handler so that a synchronous flush request from
/// the tracer also drains any buffered span stats.
pub async fn flush_all_stats_now(
    state: &Arc<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
) {
    let states: Vec<Arc<SpanConcentratorState>> = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.values().cloned().collect()
    };

    for s in states {
        let endpoint = s.endpoint.clone();
        // flush_interval is irrelevant for a one-shot send; use a dummy value.
        let exporter = make_exporter(&s, endpoint, Duration::from_secs(10));
        if let Err(e) = exporter.send(false).await {
            warn!("flush_all_stats_now: failed to send stats: {e}");
        }
    }
}
