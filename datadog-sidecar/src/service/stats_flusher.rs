// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Periodic stats flusher for the SHM span concentrator.
//!
//! The sidecar maintains one `SpanConcentratorState` per env (globally, across all sessions)
//! in `SidecarServer::span_concentrators` (a `HashMap<ConcentratorKey, SpanConcentratorState>`
//! keyed by env+version).  A tokio task holds a `Weak` reference to it and periodically calls
//! `ShmSpanConcentrator::flush`, then msgpack-encodes the result and POSTs it to the agent's
//! `/v0.6/stats` endpoint.  The first session_id that triggers creation for a given env is used
//! as the runtime_id in the stats payload for that env.

use datadog_ipc::shm_stats::{
    ShmSpanConcentrator, DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, RELOAD_FILL_RATIO,
};
use http::uri::PathAndQuery;
use http::{Method, Request};
use libdd_common::http_common::{new_client_periodic, Body};
use libdd_common::{Endpoint, HttpClient};
use libdd_trace_protobuf::pb;
use std::collections::{HashMap, VecDeque};
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

/// The subset of session configuration needed to create and flush a span stats concentrator.
#[derive(Clone)]
pub(crate) struct StatsConfig {
    pub endpoint: Endpoint,
    pub tracer_version: String,
    pub flush_interval: Duration,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Map key for the per-(env, version) concentrator map.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct ConcentratorKey {
    pub env: String,
    pub version: String,
}

/// State held per-(env, version) for SHM span stats.
pub struct SpanConcentratorState {
    pub concentrator: ShmSpanConcentrator,
    pub path: CString,
    /// Number of live `SpanConcentratorGuard`s referring to this entry.
    pub(crate) ref_count: Arc<AtomicUsize>,
    /// Unix timestamp (seconds) when `ref_count` last dropped to zero; `u64::MAX` while active.
    pub(crate) last_zero_secs: Arc<AtomicU64>,
    /// Fields needed for both the periodic flush loop and on-demand synchronous flushes.
    pub(crate) tracer_version: String,
    pub(crate) runtime_id: String,
    pub(crate) endpoint: Endpoint,
}

// SAFETY: ShmSpanConcentrator is designed for cross-process sharing; all internal state
// uses atomic operations.  The Mutex in SessionInfo guards exclusive sidecar access.
unsafe impl Send for SpanConcentratorState {}

/// RAII guard that keeps an (env, version) concentrator alive.
///
/// Stored in `ActiveApplication`.  When the last guard for a given (env, version) is dropped,
/// the flush loop will remove the concentrator after `IDLE_REMOVE_SECS` seconds.
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

/// Compute the SHM path for an (env, version) pair's span concentrator.
///
/// Uses the same scheme as `agent_remote_config.rs` and `agent_info.rs`:
/// `/ddspsc-{uid}-{hash(env+version)}`, truncated to 31 chars (macOS limit).
pub fn env_stats_shm_path(env: &str, version: &str) -> CString {
    let mut hasher = ZwoHasher::default();
    env.hash(&mut hasher);
    version.hash(&mut hasher);
    let mut path = format!(
        "/ddspsc-{}-{}",
        crate::primary_sidecar_identifier(),
        hasher.finish()
    );
    path.truncate(31);
    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

/// Build the `/v0.6/stats` URI from an endpoint, or `None` for agentless (has API key).
fn stats_uri(endpoint: &Endpoint) -> Option<http::Uri> {
    if endpoint.api_key.is_some() {
        return None; // skip stats for agentless mode
    }
    let mut parts = endpoint.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static("/v0.6/stats"));
    http::Uri::from_parts(parts).ok()
}

/// Send a serialized `ClientStatsPayload` as msgpack to the agent.
///
/// Returns `true` on success or a non-retryable failure (e.g., serialization error or agent
/// rejection); returns `false` on a transient network/connection error so the caller can retry.
async fn send_stats(
    client: &HttpClient,
    uri: &http::Uri,
    endpoint: &Endpoint,
    payload: &pb::ClientStatsPayload,
) -> bool {
    let bytes = match rmp_serde::to_vec_named(payload) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to serialize stats payload: {e}");
            return true; // non-retryable
        }
    };
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(uri.clone())
        .header("Content-Type", "application/msgpack");
    for (name, value) in endpoint.get_optional_headers() {
        builder = builder.header(name, value);
    }
    let req = match builder.body(Body::from(bytes)) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to build stats request: {e}");
            return true; // non-retryable
        }
    };
    match client.request(req).await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                warn!("Agent rejected stats payload (status {status})");
            }
            true
        }
        Err(e) => {
            warn!("Failed to send stats to agent: {e}");
            false // transient — caller should retry
        }
    }
}

/// Maximum number of stats payloads to buffer for retry before dropping the oldest.
const MAX_PENDING_STATS: usize = 10;

/// Spawn-and-forget flush loop for an (env, version) pair's SHM span concentrator.
///
/// The loop exits when the `Weak` can no longer be upgraded (sidecar shutting down), when the
/// entry for this key is removed from the map, or when the concentrator has been idle (no active
/// `SpanConcentratorGuard`s) for `IDLE_REMOVE_SECS` seconds.
///
/// On transient send failures the payload is retained in `pending` and retried on the next
/// tick, so stats are not silently dropped when the agent is temporarily unreachable at startup.
///
/// The endpoint (including test-session token) is read from `SpanConcentratorState` on every
/// tick so that late endpoint updates (e.g. a test-session token set after concentrator creation)
/// are picked up automatically.
pub async fn run_stats_flush_loop(
    state: Weak<Mutex<HashMap<ConcentratorKey, SpanConcentratorState>>>,
    map_key: ConcentratorKey,
    flush_interval: Duration,
) {
    let client = new_client_periodic();
    // Payloads that failed to send on a previous tick and should be retried.
    let mut pending: VecDeque<(pb::ClientStatsPayload, http::Uri, Endpoint)> = VecDeque::new();
    loop {
        tokio::time::sleep(flush_interval).await;
        let Some(arc) = state.upgrade() else {
            break; // sidecar shutting down, stop flushing
        };

        // Regular flush — always drain the SHM concentrator to prevent it from filling up.
        // Read the endpoint fresh each tick so that updates (e.g., a test-session token added
        // after the concentrator was created) are reflected immediately.
        let tick_result = {
            let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = guard.get(&map_key) else {
                break; // concentrator was removed, stop
            };
            let uri = stats_uri(&s.endpoint);
            let (used, total) = s.concentrator.slot_usage();
            if total > 0 {
                let fill = used as f64 / total as f64;
                if fill > RELOAD_FILL_RATIO {
                    warn!(
                        "SHM span concentrator for env={} version={} is {:.0}% full \
                         ({used}/{total} slots); consider increasing slot count",
                        map_key.env,
                        map_key.version,
                        fill * 100.0
                    );
                }
            }
            let payload = s.concentrator.flush(
                false,
                "",
                &map_key.env,
                &map_key.version,
                "",
                &s.tracer_version,
                &s.runtime_id,
                "",
            );
            (payload, uri, s.endpoint.clone())
        };
        let (new_payload, uri_opt, endpoint) = tick_result;
        let Some(uri) = uri_opt else {
            continue; // agentless — skip
        };

        if let Some(payload) = new_payload {
            if pending.len() >= MAX_PENDING_STATS {
                warn!(
                    "Stats send backlog full for env={} version={}; dropping oldest payload",
                    map_key.env, map_key.version,
                );
                pending.pop_front();
            }
            pending.push_back((payload, uri.clone(), endpoint.clone()));
        }

        // Stop on the first transient failure to avoid sending newer data out of order.
        // Use the current endpoint (with up-to-date headers) for all retries.
        let mut sent = 0;
        for (p, _stored_uri, _stored_ep) in &pending {
            if send_stats(&client, &uri, &endpoint, p).await {
                sent += 1;
            } else {
                break;
            }
        }
        pending.drain(..sent);

        // Idle-removal check: if no app has held a guard for >= IDLE_REMOVE_SECS, retire this
        // concentrator with a final force-flush.
        let Some(arc) = state.upgrade() else {
            break;
        };
        let final_result = {
            let mut map_guard = arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = map_guard.get(&map_key) else {
                break; // already removed by someone else
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
                map_guard.remove(&map_key).map(|s| {
                    info!(
                        "Removing idle SHM span concentrator for env={} version={} \
                         (idle for {idle_secs}s)",
                        map_key.env, map_key.version,
                    );
                    let uri = stats_uri(&s.endpoint);
                    let ep = s.endpoint.clone();
                    let payload = s.concentrator.flush(
                        true,
                        "",
                        &map_key.env,
                        &map_key.version,
                        "",
                        &s.tracer_version,
                        &s.runtime_id,
                        "",
                    );
                    (payload, uri, ep)
                })
            } else {
                None
            }
        };
        if let Some((payload, uri_opt, ep)) = final_result {
            if let (Some(payload), Some(uri)) = (payload, uri_opt) {
                send_stats(&client, &uri, &ep, &payload).await;
            }
            break; // concentrator was removed above
        }
    }
}

/// Create (or look up) the SHM span concentrator for an (env, version) pair, increment its
/// reference count, and return a guard.
///
/// Idempotent with respect to SHM creation: if a concentrator for this (env, version) already
/// exists, only the reference count is incremented.
///
/// Returns `None` when no `SessionConfig` has been set yet for the calling session (caller should
/// retry later) or when SHM creation fails.
///
/// - `concentrators`: the global per-(env,version) map from `SidecarServer::span_concentrators`
/// - `env`: the environment name
/// - `version`: the application version
/// - `runtime_id`: used as runtime_id in flush payloads (only meaningful for the first caller)
/// - `session`: the calling session (provides `SessionConfig`)
pub(crate) fn ensure_stats_concentrator(
    concentrators: &Arc<Mutex<HashMap<ConcentratorKey, SpanConcentratorState>>>,
    env: &str,
    version: &str,
    runtime_id: &str,
    session: &crate::service::session_info::SessionInfo,
) -> Option<SpanConcentratorGuard> {
    let config = session
        .stats_config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()?;

    let map_key = ConcentratorKey {
        env: env.to_owned(),
        version: version.to_owned(),
    };
    let mut guard = concentrators.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(s) = guard.get_mut(&map_key) {
        // Concentrator already exists — increment ref count and reset idle timer.
        s.last_zero_secs.store(u64::MAX, Release);
        s.ref_count.fetch_add(1, AcqRel);
        // Always update the endpoint so that a later session with a test-session token
        // (e.g. the actual test after the SKIPIF check ran without one) takes effect before
        // the next flush tick.
        s.endpoint = config.endpoint.clone();
        return Some(SpanConcentratorGuard {
            ref_count: s.ref_count.clone(),
            last_zero_secs: s.last_zero_secs.clone(),
        });
    }

    let path = env_stats_shm_path(env, version);
    let bucket_nanos: u64 = 10_000_000_000; // 10 s
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
            let tracer_version = config.tracer_version.clone();
            let rid = runtime_id.to_owned();
            guard.insert(
                map_key.clone(),
                SpanConcentratorState {
                    concentrator,
                    path,
                    ref_count,
                    last_zero_secs,
                    tracer_version: tracer_version.clone(),
                    runtime_id: rid.clone(),
                    endpoint: config.endpoint.clone(),
                },
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
    state: &Arc<Mutex<HashMap<ConcentratorKey, SpanConcentratorState>>>,
) {
    // Collect all payloads while holding the lock (flush is &self — atomic ops only).
    let payloads: Vec<(http::Uri, Endpoint, pb::ClientStatsPayload)> = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .filter_map(|(key, s)| {
                let uri = stats_uri(&s.endpoint)?;
                let payload = s.concentrator.flush(
                    false,
                    "",
                    &key.env,
                    &key.version,
                    "",
                    &s.tracer_version,
                    &s.runtime_id,
                    "",
                )?;
                Some((uri, s.endpoint.clone(), payload))
            })
            .collect()
    };

    if payloads.is_empty() {
        return;
    }

    let client = new_client_periodic();
    for (uri, endpoint, payload) in payloads {
        send_stats(&client, &uri, &endpoint, &payload).await;
    }
}
