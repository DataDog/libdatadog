// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Periodic stats flusher for the SHM span concentrator.
//!
//! The sidecar maintains one `SpanConcentratorState` per (env, version, service) triple
//! (globally, across all sessions) in `SidecarServer::span_concentrators`
//! (a `HashMap<ConcentratorKey, SpanConcentratorState>`).  A tokio task holds a `Weak`
//! reference to it and periodically calls `ShmSpanConcentrator::flush`, then msgpack-encodes
//! the result and POSTs it to the agent's `/v0.6/stats` endpoint.  The first session_id that
//! triggers creation for a given (env, version, service) is used as the runtime_id in the
//! stats payload for that key.

use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::shm_stats::{
    ShmSpanConcentrator, DEFAULT_SLOT_COUNT, DEFAULT_STRING_POOL_BYTES, RELOAD_FILL_RATIO,
};
use http::uri::PathAndQuery;
use http::{Method, Request};
use libdd_common::http_common::{new_client_periodic, Body};
use libdd_common::tag::Tag;
use libdd_common::{Endpoint, HttpClient};
use libdd_dogstatsd_client::DogStatsDActionOwned;
use libdd_trace_protobuf::pb;
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use zwohash::ZwoHasher;

/// Detect the current machine hostname via `gethostname(2)`.  Returns an empty string on error.
pub(crate) fn get_hostname() -> String {
    let mut buf = vec![0u8; 256];
    // SAFETY: buf is valid for the given length; gethostname writes a NUL-terminated string.
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if ret != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

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
    /// Language identifier (e.g. "php") — included as `lang` base tag in DogStatsD metrics.
    pub language: String,
    /// Tracer library version — included as `tracer_version` base tag in DogStatsD metrics.
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
    /// Fields needed for both the periodic flush loop and on-demand synchronous flushes.
    pub(crate) runtime_id: String,
    /// Wrapped in Mutex for interior mutability: the endpoint (incl. test-session token) is
    /// updated each time a new session reconnects for the same (env, version, service).
    pub(crate) endpoint: Mutex<Endpoint>,
    /// Hostname of the machine running this PHP process (populated once at concentrator creation).
    pub(crate) hostname: String,
    /// Process-level tags serialised as `"key:value,..."`, forwarded to the stats payload.
    pub(crate) process_tags: String,
    /// Language identifier sent in the `Datadog-Tracer-Language` request header.
    pub(crate) language: String,
    /// Tracer version sent in the `Datadog-Tracer-Version` request header.
    pub(crate) tracer_version: String,
    /// Shared DogStatsD client (cloned Arc from the session that created this concentrator).
    /// Used to emit tracer self-observability metrics without allocating a new UDP socket.
    pub(crate) dogstatsd: Arc<Mutex<Option<libdd_dogstatsd_client::Client>>>,
    /// Base tags (`env`, `lang`, `tracer_version`) shared across all DogStatsD metrics.
    pub(crate) base_tags: Vec<Tag>,
}

// SAFETY: ShmSpanConcentrator is designed for cross-process sharing; all internal state
// uses atomic operations.  The Mutex in SessionInfo guards exclusive sidecar access.
unsafe impl Send for SpanConcentratorState {}

impl SpanConcentratorState {
    /// Flush the SHM concentrator and stamp the returned payload with `process_tags`.
    ///
    /// Returns `None` when the concentrator has no data to report.
    fn flush_payload(&self, force: bool, key: &ConcentratorKey) -> Option<pb::ClientStatsPayload> {
        let mut payload = self.concentrator.flush(
            force,
            self.hostname.clone(),
            key.env.clone(),
            key.version.clone(),
            key.root_service.clone(),
            self.runtime_id.clone(),
        )?;
        payload.process_tags = self.process_tags.clone();
        Some(payload)
    }

    /// Send a single payload and emit the corresponding DogStatsD metrics.
    ///
    /// Used for one-shot flushes (idle-removal, `flush_all_stats_now`).  The retry-accumulator
    /// path in `run_stats_flush_loop` has its own send loop and does not use this.
    async fn send_and_emit(&self, client: &HttpClient, payload: pb::ClientStatsPayload) {
        let endpoint = self
            .endpoint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let spans = spans_in_payload(&payload);
        let buckets = payload.stats.len() as i64;
        match send_stats(
            client,
            &endpoint,
            &payload,
            self.language.clone(),
            self.tracer_version.clone(),
        )
        .await
        {
            StatsSendResult::Sent => {
                emit_flush_metrics(&self.dogstatsd, &self.base_tags, spans, 1, buckets, 0)
            }
            StatsSendResult::Error | StatsSendResult::Network => {
                emit_flush_metrics(&self.dogstatsd, &self.base_tags, 0, 0, 0, 1)
            }
        }
    }
}

/// RAII guard that keeps an (env, version, root-service) concentrator alive.
///
/// Stored in `ActiveApplication`.  When the last guard for a given (env, version, root-service) is
/// dropped, the flush loop will remove the concentrator after `IDLE_REMOVE_SECS` seconds.
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

/// Result of a single stats payload send attempt.
#[must_use]
enum StatsSendResult {
    /// Agent accepted the payload (2xx response).
    Sent,
    /// Non-retryable failure: serialization error or HTTP error response from the agent.
    Error,
    /// Transient network failure — payload should be kept in the retry queue.
    Network,
}

/// Send a serialized `ClientStatsPayload` as msgpack to the agent.
///
/// `endpoint` must already have the `/v0.6/stats` path set (use `stats_endpoint`).
async fn send_stats(
    client: &HttpClient,
    endpoint: &Endpoint,
    payload: &pb::ClientStatsPayload,
    language: String,
    tracer_version: String,
) -> StatsSendResult {
    let bytes = match rmp_serde::to_vec_named(payload) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to serialize stats payload: {e}");
            return StatsSendResult::Error;
        }
    };
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(endpoint.url.clone())
        .header("Content-Type", "application/msgpack")
        .header("Datadog-Tracer-Language", language)
        .header("Datadog-Tracer-Version", tracer_version);
    for (name, value) in endpoint.get_optional_headers() {
        builder = builder.header(name, value);
    }
    let req = match builder.body(Body::from(bytes)) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to build stats request: {e}");
            return StatsSendResult::Error;
        }
    };
    match client.request(req).await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                StatsSendResult::Sent
            } else {
                warn!("Agent rejected stats payload (status {status})");
                StatsSendResult::Error
            }
        }
        Err(e) => {
            warn!("Failed to send stats to agent: {e}");
            StatsSendResult::Network
        }
    }
}

/// Sum of all span hits across every group in every bucket of a payload.
/// Used to populate the `datadog.tracer.stats.spans_in` metric.
fn spans_in_payload(payload: &pb::ClientStatsPayload) -> i64 {
    payload
        .stats
        .iter()
        .flat_map(|b| b.stats.iter())
        .map(|g| g.hits as i64)
        .sum()
}

/// Emit DogStatsD self-observability metrics for a stats flush cycle.
///
/// All counters are emitted with `env`, `lang`, and `tracer_version` base tags.
/// No-ops when the DogStatsD client is not configured or all counts are zero.
fn emit_flush_metrics(
    dogstatsd: &Arc<Mutex<Option<libdd_dogstatsd_client::Client>>>,
    base_tags: &[Tag],
    spans_in: i64,
    payloads_sent: i64,
    buckets_sent: i64,
    errors: i64,
) {
    let guard = dogstatsd.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ref ds) = *guard else { return };
    let tags = base_tags.to_vec();
    let mut actions: Vec<DogStatsDActionOwned> = Vec::with_capacity(4);
    if spans_in > 0 {
        actions.push(DogStatsDActionOwned::Count(
            "datadog.tracer.stats.spans_in".into(),
            spans_in,
            tags.clone(),
        ));
    }
    if payloads_sent > 0 {
        actions.push(DogStatsDActionOwned::Count(
            "datadog.tracer.stats.flush_payloads".into(),
            payloads_sent,
            tags.clone(),
        ));
        actions.push(DogStatsDActionOwned::Count(
            "datadog.tracer.stats.flush_buckets".into(),
            buckets_sent,
            tags.clone(),
        ));
    }
    if errors > 0 {
        actions.push(DogStatsDActionOwned::Count(
            "datadog.tracer.stats.flush_errors".into(),
            errors,
            tags,
        ));
    }
    if !actions.is_empty() {
        ds.send_owned(actions);
    }
}

/// Maximum number of stats payloads to buffer for retry before dropping the oldest.
const MAX_PENDING_STATS: usize = 10;

/// Spawn-and-forget flush loop for an (env, version, root-service) pair's SHM span concentrator.
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
    state: Weak<Mutex<HashMap<ConcentratorKey, Arc<SpanConcentratorState>>>>,
    map_key: ConcentratorKey,
    flush_interval: Duration,
) {
    let client = new_client_periodic();
    // Payloads that failed to send on a previous tick and should be retried.
    // Only the payload itself is stored — the endpoint is read fresh every tick so
    // test-session tokens and other late updates are always applied on retry too.
    let mut pending: VecDeque<pb::ClientStatsPayload> = VecDeque::new();
    loop {
        tokio::time::sleep(flush_interval).await;
        let Some(arc) = state.upgrade() else {
            break; // sidecar shutting down, stop flushing
        };

        // Grab the Arc under the lock, then release before doing any SHM work.
        let s = {
            let guard = arc.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = guard.get(&map_key) else {
                break; // concentrator was removed, stop
            };
            s.clone()
        };

        // Flush and fill-check outside the lock — both are atomic SHM operations.
        let (used, total) = s.concentrator.slot_usage();
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
        let new_payload = s.flush_payload(false, &map_key);

        if let Some(payload) = new_payload {
            if pending.len() >= MAX_PENDING_STATS {
                warn!(
                    "Stats send backlog full for env={} version={}; dropping oldest payload",
                    map_key.env, map_key.version,
                );
                pending.pop_front();
            }
            pending.push_back(payload);
        }

        // Stop on the first transient failure to avoid sending newer data out of order.
        // Use the current endpoint (with up-to-date headers) for all retries.
        let mut to_drain = 0usize;
        let mut payloads_sent = 0i64;
        let mut buckets_sent = 0i64;
        let mut spans_sent = 0i64;
        let mut errors = 0i64;
        let endpoint = s.endpoint.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for p in &pending {
            match send_stats(
                &client,
                &endpoint,
                &p,
                s.language.to_owned(),
                s.tracer_version.to_owned(),
            )
            .await
            {
                StatsSendResult::Sent => {
                    to_drain += 1;
                    payloads_sent += 1;
                    buckets_sent += p.stats.len() as i64;
                    spans_sent += spans_in_payload(p);
                }
                StatsSendResult::Error => {
                    to_drain += 1; // non-retryable: drop from queue
                    errors += 1;
                }
                StatsSendResult::Network => {
                    errors += 1;
                    break; // keep remaining in queue for next tick
                }
            }
        }
        pending.drain(..to_drain);
        emit_flush_metrics(
            &s.dogstatsd,
            &s.base_tags,
            spans_sent,
            payloads_sent,
            buckets_sent,
            errors,
        );

        // Idle-removal check: if no app has held a guard for >= IDLE_REMOVE_SECS, retire this
        // concentrator with a final force-flush.
        let Some(arc) = state.upgrade() else {
            break;
        };
        // Idle-removal: check under the lock, remove if stale, then flush outside.
        let removed = {
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
                info!(
                    "Removing idle SHM span concentrator for env={} version={} service={} \
                     (idle for {idle_secs}s)",
                    map_key.env, map_key.version, map_key.root_service,
                );
                map_guard.remove(&map_key)
            } else {
                None
            }
        };
        if let Some(s) = removed {
            if let Some(payload) = s.flush_payload(true, &map_key) {
                s.send_and_emit(&client, payload).await;
            }
            break; // concentrator was removed above
        }
    }
}

/// Create (or look up) the SHM span concentrator for an (env, service, version) pair, increment its
/// reference count, and return a guard.
///
/// Idempotent with respect to SHM creation: if a concentrator for this (env, service, version)
/// already exists, only the reference count is incremented.
///
/// Returns `None` when no `SessionConfig` has been set yet for the calling session (caller should
/// retry later) or when SHM creation fails.
///
/// - `concentrators`: the global per-(env,version,service) map from
///   `SidecarServer::span_concentrators`
/// - `env`: the environment name
/// - `version`: the application version
/// - `service_name`: the root service name reported by `set_universal_service_tags`
/// - `runtime_id`: used as runtime_id in flush payloads (only meaningful for the first caller)
/// - `session`: the calling session (provides `StatsConfig`)
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
    let stats_ep = config.endpoint.clone();

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
        // Always update the endpoint so that a later session with a test-session token
        // (e.g. the actual test after the SKIPIF check ran without one) takes effect before
        // the next flush tick.
        *s.endpoint.lock().unwrap_or_else(|e| e.into_inner()) = stats_ep;
        return Some(SpanConcentratorGuard {
            ref_count: s.ref_count.clone(),
            last_zero_secs: s.last_zero_secs.clone(),
        });
    }

    let path = env_stats_shm_path(env, version, service_name);
    let bucket_nanos: u64 = 10_000_000_000; // 10 s

    let base_tags: Vec<Tag> = [
        Tag::new("env", env),
        Tag::new("lang", &config.language),
        Tag::new("tracer_version", &config.tracer_version),
    ]
    .into_iter()
    .filter_map(|r| r.ok())
    .collect();

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
                    runtime_id: runtime_id.to_owned(),
                    endpoint: Mutex::new(stats_ep),
                    hostname: config.hostname.clone(),
                    process_tags: config.process_tags.clone(),
                    language: config.language.clone(),
                    tracer_version: config.tracer_version.clone(),
                    dogstatsd: session.clone_dogstatsd(),
                    base_tags,
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
    // Collect (key, state) pairs under the lock, then release before any SHM or I/O work.
    let states: Vec<(ConcentratorKey, Arc<SpanConcentratorState>)> = {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        guard.iter().map(|(k, s)| (k.clone(), s.clone())).collect()
    };

    if states.is_empty() {
        return;
    }

    let client = new_client_periodic();
    for (key, s) in states {
        if let Some(payload) = s.flush_payload(false, &key) {
            s.send_and_emit(&client, payload).await;
        }
    }
}
