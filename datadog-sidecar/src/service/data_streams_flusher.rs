// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Periodic flusher for the SHM Data Streams Monitoring (DSM) stats tables.
//!
//! The sidecar maintains one `DsmTableState` per [`DsmTableKey`] (globally, across all
//! sessions) in `SidecarServer::dsm_tables`. Tables are created lazily on the first IPC stats
//! point for a given key, and removed automatically once idle, the same way as the span stats
//! concentrators: an empty drain tells PHP workers to stop writing, and a final drain follows
//! before removal.
//!
//! Each key has two tables: the stats table, and the backlog table holding the highest offset
//! seen per backlog (Kafka partition, consumer group). Both are drained once per
//! [`DSM_BUCKET_NANOS`] interval, just after its boundary, and sent together to the agent's
//! `/v0.1/pipeline_stats` endpoint as the gzipped msgpack payload the Java and Go tracers send.

use crate::service::connection_session::ConnectionSession;
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::write::GzEncoder;
use futures::future::join_all;
use http::uri::PathAndQuery;
use libdd_capabilities_impl::{HttpClientCapability, NativeCapabilities};
use libdd_common::{Endpoint, MutexExt};
use libdd_ipc::shm_dsm::{
    DEFAULT_DSM_SLOT_COUNT, DEFAULT_DSM_STRING_POOL_BYTES, DSM_BUCKET_NANOS, DsmStatsBucket,
    ShmDsmTable, now_nanos,
};
use libdd_ipc::shm_dsm_backlog::{
    DEFAULT_DSM_BACKLOG_SLOT_COUNT, DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES, DsmBacklog,
    DsmBacklogBucket, ShmDsmBacklogTable,
};
use libdd_trace_utils::send_with_retry::{
    CompressionStrategy, RetryBackoffType, RetryStrategy, send_with_retry,
};
use libdd_trace_utils::tracer_header_tags::TracerHeaderTags;
use serde::{Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, error, info, warn};
use zwohash::ZwoHasher;

/// Path of the agent's DSM endpoint.
pub const PIPELINE_STATS_ENDPOINT_PATH: &str = "/v0.1/pipeline_stats";

/// `ProductMask` bits, as the Java and Go tracers define them.
const PRODUCT_APM: u64 = 1;
const PRODUCT_DSM: u64 = 2;

/// How long after an interval boundary its bucket is drained, so that points recorded just
/// before the boundary have reached it.
const DRAIN_GRACE: Duration = Duration::from_millis(50);

/// Build the DSM endpoint by appending `/v0.1/pipeline_stats` to the agent base URL.
/// Returns `None` for agentless mode (API key present): DSM is only sent through the agent.
pub(crate) fn dsm_endpoint(endpoint: &Endpoint) -> Option<Endpoint> {
    if endpoint.api_key.is_some() {
        return None;
    }
    let mut parts = endpoint.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(PIPELINE_STATS_ENDPOINT_PATH));
    Some(Endpoint {
        url: http::Uri::from_parts(parts).ok()?,
        ..endpoint.clone()
    })
}

/// The subset of session configuration needed to create and flush a DSM stats table.
#[derive(Clone)]
pub(crate) struct DsmConfig {
    /// DSM endpoint with final path already baked in.
    pub endpoint: Endpoint,
    /// Language identifier (e.g. "php").
    pub language: String,
    pub language_version: String,
    /// Tracer library version.
    pub tracer_version: String,
    /// `key:value` process tags, as the payload's `ProcessTags` carries them.
    pub process_tags: Vec<String>,
}

/// Map key for the DSM table map: one table per payload the tables are flushed into.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct DsmTableKey {
    pub env: String,
    pub version: String,
    /// The service the points are reported under, including any service override.
    pub service: String,
    pub primary_tag: String,
}

/// Compute the SHM path for a DSM stats table.
pub fn dsm_table_shm_path(key: &DsmTableKey) -> CString {
    shm_path("dddsm", key)
}

/// Compute the SHM path for the DSM backlog table kept alongside a stats table.
pub fn dsm_backlog_shm_path(key: &DsmTableKey) -> CString {
    shm_path("dddsb", key)
}

/// `prefix` tells a key's tables apart; it is not hashed, so that it keeps the stats table's
/// path what it was.
fn shm_path(prefix: &str, key: &DsmTableKey) -> CString {
    let mut hasher = ZwoHasher::default();
    key.env.hash(&mut hasher);
    key.version.hash(&mut hasher);
    key.service.hash(&mut hasher);
    key.primary_tag.hash(&mut hasher);
    let hash = hasher.finish();

    let mut path = format!(
        "/{prefix}{}-{}",
        crate::shm_namespace(),
        BASE64_URL_SAFE_NO_PAD.encode(hash.to_ne_bytes()),
    );
    if cfg!(unix) {
        path.truncate(31);
    }
    #[allow(clippy::unwrap_used)]
    CString::new(path).unwrap()
}

/// State held per [`DsmTableKey`].
pub struct DsmTableState {
    pub table: ShmDsmTable,
    pub backlogs: ShmDsmBacklogTable,
    key: DsmTableKey,
    config: DsmConfig,
}

// SAFETY: ShmDsmTable and ShmDsmBacklogTable are designed for cross-process sharing; all
// internal state uses atomic operations.
unsafe impl Send for DsmTableState {}

pub(crate) type DsmTables = Arc<Mutex<HashMap<DsmTableKey, Arc<DsmTableState>>>>;

/// Look up or create the SHM DSM tables for `key`.
///
/// Called lazily when a PHP worker sends a stats point or a backlog over IPC because it could
/// not write it to SHM directly (no table yet). Both tables are created together, so that a
/// worker that can open one can open the other. Returns `None` when DSM is not available for
/// the session (agentless).
pub(crate) fn get_or_create_dsm_table(
    tables: &DsmTables,
    key: DsmTableKey,
    session: &ConnectionSession,
) -> Option<Arc<DsmTableState>> {
    let config = session.dsm_config.clone()?;
    let mut guard = tables.lock_or_panic();

    if let Some(s) = guard.get(&key) {
        if !s.table.needs_reload() {
            return Some(s.clone());
        }
        // Entry is being retired (reload signalled) — fall through to create a fresh one.
    }

    let created = ShmDsmBacklogTable::create(
        dsm_backlog_shm_path(&key),
        DSM_BUCKET_NANOS,
        DEFAULT_DSM_BACKLOG_SLOT_COUNT,
        DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES,
    )
    .and_then(|backlogs| {
        let table = ShmDsmTable::create(
            dsm_table_shm_path(&key),
            DSM_BUCKET_NANOS,
            DEFAULT_DSM_SLOT_COUNT,
            DEFAULT_DSM_STRING_POOL_BYTES,
        )
        .inspect_err(|_| backlogs.unlink())?;
        Ok((table, backlogs))
    });
    match created {
        Ok((table, backlogs)) => {
            let state = Arc::new(DsmTableState {
                table,
                backlogs,
                key: key.clone(),
                config,
            });
            guard.insert(key.clone(), state.clone());
            let weak = Arc::downgrade(tables);
            tokio::spawn(async move {
                run_dsm_flush_loop(weak, key).await;
            });
            Some(state)
        }
        Err(e) => {
            error!(
                "Failed to create SHM DSM table for env={} version={} service={}: {e}",
                key.env, key.version, key.service
            );
            None
        }
    }
}

/// Time from `now` until just after the next interval boundary.
fn until_next_drain(now: SystemTime) -> Duration {
    let now = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    Duration::from_nanos(DSM_BUCKET_NANOS - now % DSM_BUCKET_NANOS) + DRAIN_GRACE
}

/// Spawn-and-forget flush loop for a DSM table.
///
/// **Idle removal**: when a drain produces no data in either table, the reload bit is set on
/// both, signalling PHP workers to stop writing (they will fall back to the IPC path). After a
/// grace period, a final forced drain sends any remaining data and the tables are removed from
/// the map.
pub(crate) async fn run_dsm_flush_loop(
    tables: Weak<Mutex<HashMap<DsmTableKey, Arc<DsmTableState>>>>,
    key: DsmTableKey,
) {
    let Some(arc) = tables.upgrade() else {
        return;
    };
    let state = {
        let guard = arc.lock_or_panic();
        guard.get(&key).cloned()
    };
    drop(arc);
    let Some(state) = state else {
        return;
    };
    let client = NativeCapabilities::new_client();

    loop {
        tokio::time::sleep(until_next_drain(SystemTime::now())).await;
        let Some(arc) = tables.upgrade() else {
            break; // sidecar shutting down
        };

        let drained = state.drain(false);
        if !drained.is_empty() {
            if let Err(e) = send_buckets(&client, &state, drained).await {
                warn!(
                    "Failed to send DSM stats for env={} version={} service={}: {e}",
                    key.env, key.version, key.service
                );
            }
            continue;
        }

        // Empty drain: retire this table.
        info!(
            "Removing idle SHM DSM table for env={} version={} service={}",
            key.env, key.version, key.service
        );
        state.table.signal_reload();
        state.backlogs.signal_reload();
        state.table.unlink();
        state.backlogs.unlink();
        tokio::time::sleep(Duration::from_secs(1)).await;
        {
            let mut guard = arc.lock_or_panic();
            // Only remove our entry — a fresher one may have been inserted already.
            if guard.get(&key).is_some_and(|s| Arc::ptr_eq(s, &state)) {
                guard.remove(&key);
            }
        }
        let drained = state.drain(true);
        if !drained.is_empty() {
            if let Err(e) = send_buckets(&client, &state, drained).await {
                warn!("Failed final DSM stats flush: {e}");
            }
        }
        break;
    }
}

/// Immediately drain all active DSM tables, including the intervals still under way, and send
/// the results to the agent.
pub async fn flush_all_dsm_now(tables: &DsmTables) {
    let states: Vec<_> = tables.lock_or_panic().values().cloned().collect();
    debug!("Flushing all DSM stats now: {} tables", states.len());
    let client = NativeCapabilities::new_client();
    join_all(states.iter().map(|state| {
        let client = &client;
        async move {
            let drained = state.drain(true);
            if drained.is_empty() {
                return;
            }
            if let Err(e) = send_buckets(client, state, drained).await {
                warn!("flush_all_dsm_now: failed to send DSM stats: {e}");
            }
        }
    }))
    .await;
}

/// What one drain of a key's two tables took out of them.
struct Drained {
    stats: Vec<DsmStatsBucket>,
    backlogs: Vec<DsmBacklogBucket>,
}

impl Drained {
    fn is_empty(&self) -> bool {
        self.stats.is_empty() && self.backlogs.is_empty()
    }
}

impl DsmTableState {
    /// Drain both tables as of one instant, so that the buckets of an interval share a start.
    fn drain(&self, force: bool) -> Drained {
        let now = now_nanos();
        Drained {
            stats: self.table.drain_buckets_at(force, now),
            backlogs: self.backlogs.drain_buckets_at(force, now),
        }
    }
}

async fn send_buckets(
    client: &NativeCapabilities,
    state: &DsmTableState,
    drained: Drained,
) -> anyhow::Result<()> {
    let body = gzip(&encode_payload(&state.key, &state.config, &drained)?)?;

    let config = &state.config;
    let mut headers: http::HeaderMap = TracerHeaderTags {
        lang: &config.language,
        lang_version: &config.language_version,
        tracer_version: &config.tracer_version,
        ..Default::default()
    }
    .into();
    headers.insert(
        http::header::CONTENT_TYPE,
        libdd_common::header::APPLICATION_MSGPACK,
    );
    headers.insert(
        http::header::CONTENT_ENCODING,
        http::HeaderValue::from_static("gzip"),
    );

    // Already gzipped above: the endpoint requires gzip, which the retry helper does not offer.
    send_with_retry(
        client,
        &config.endpoint,
        body,
        &headers,
        &RetryStrategy::new(2, 100, RetryBackoffType::Exponential, None),
        CompressionStrategy::None,
    )
    .await
    .map(|_| ())
    .map_err(|e| anyhow::anyhow!("{e:?}"))
}

fn gzip(payload: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload)?;
    encoder.finish()
}

/// Bytes serialized as msgpack `bin`, as the payload carries sketches.
struct Bin<'a>(&'a [u8]);

impl Serialize for Bin<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

/// The payload the Java and Go tracers send to `/v0.1/pipeline_stats`; fields are named as
/// theirs are.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StatsPayload<'a> {
    env: &'a str,
    service: &'a str,
    version: &'a str,
    lang: &'a str,
    tracer_version: &'a str,
    primary_tag: &'a str,
    product_mask: u64,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    process_tags: &'a [String],
    stats: Vec<StatsBucket<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StatsBucket<'a> {
    start: u64,
    duration: u64,
    stats: Vec<StatsPoint<'a>>,
    /// As in the Java tracer, a bucket without backlogs carries no `Backlogs` at all.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    backlogs: Vec<Backlog<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Backlog<'a> {
    tags: &'a [String],
    value: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct StatsPoint<'a> {
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    edge_tags: &'a [String],
    hash: u64,
    parent_hash: u64,
    pathway_latency: Bin<'a>,
    edge_latency: Bin<'a>,
    payload_size: Bin<'a>,
}

fn encode_payload(
    key: &DsmTableKey,
    config: &DsmConfig,
    drained: &Drained,
) -> anyhow::Result<Vec<u8>> {
    // Both tables' buckets for an interval go out as one, ordered by start.
    let mut buckets: BTreeMap<(u64, u64), StatsBucket<'_>> = BTreeMap::new();
    fn bucket<'m, 'a>(
        buckets: &'m mut BTreeMap<(u64, u64), StatsBucket<'a>>,
        start: u64,
        duration: u64,
    ) -> &'m mut StatsBucket<'a> {
        buckets
            .entry((start, duration))
            .or_insert_with(|| StatsBucket {
                start,
                duration,
                stats: vec![],
                backlogs: vec![],
            })
    }
    for b in &drained.stats {
        bucket(&mut buckets, b.start, b.duration)
            .stats
            .extend(b.stats.iter().map(|g| StatsPoint {
                edge_tags: &g.edge_tags,
                hash: g.hash,
                parent_hash: g.parent_hash,
                pathway_latency: Bin(&g.pathway_latency),
                edge_latency: Bin(&g.edge_latency),
                payload_size: Bin(&g.payload_size),
            }));
    }
    for b in &drained.backlogs {
        bucket(&mut buckets, b.start, b.duration)
            .backlogs
            .extend(b.backlogs.iter().map(|DsmBacklog { tags, value }| Backlog {
                tags,
                value: *value,
            }));
    }

    let payload = StatsPayload {
        env: &key.env,
        service: &key.service,
        version: &key.version,
        lang: &config.language,
        tracer_version: &config.tracer_version,
        primary_tag: &key.primary_tag,
        product_mask: PRODUCT_APM | PRODUCT_DSM,
        process_tags: &config.process_tags,
        stats: buckets.into_values().collect(),
    };
    Ok(rmp_serde::to_vec_named(&payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_ipc::shm_dsm::DsmStatsGroup;

    fn key() -> DsmTableKey {
        DsmTableKey {
            env: "env-a".into(),
            version: "1.0".into(),
            service: "service-a".into(),
            primary_tag: "".into(),
        }
    }

    fn config() -> DsmConfig {
        DsmConfig {
            endpoint: Endpoint::default(),
            language: "php".into(),
            language_version: "8.3".into(),
            tracer_version: "1.0.0".into(),
            process_tags: vec!["entrypoint.name:php".into()],
        }
    }

    /// A drained table goes out as one gzipped msgpack POST to the agent's DSM endpoint.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn a_drained_table_is_posted_gzipped_to_the_agent() {
        use flate2::read::GzDecoder;
        use httpmock::prelude::*;
        use libdd_ipc::shm_dsm::DsmStatsPointInput;
        use std::io::Read;

        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .path(PIPELINE_STATS_ENDPOINT_PATH)
                    .header("content-type", "application/msgpack")
                    .header("content-encoding", "gzip")
                    .header("datadog-meta-lang", "php")
                    .is_true(|request| {
                        let mut body = Vec::new();
                        GzDecoder::new(request.body_ref())
                            .read_to_end(&mut body)
                            .is_ok()
                            && rmpv::decode::read_value(&mut body.as_slice()).is_ok_and(|v| {
                                let service = v
                                    .as_map()
                                    .and_then(|m| {
                                        m.iter().find(|(k, _)| k.as_str() == Some("Service"))
                                    })
                                    .and_then(|(_, v)| v.as_str().map(str::to_owned));
                                service.as_deref() == Some("service-a")
                            })
                    });
                then.status(200);
            })
            .await;

        let table = ShmDsmTable::create(
            CString::new(format!("/dddsm-flush-t-{}", std::process::id())).unwrap(),
            DSM_BUCKET_NANOS,
            8,
            4096,
        )
        .unwrap();
        let backlogs = ShmDsmBacklogTable::create(
            CString::new(format!("/dddsb-flush-t-{}", std::process::id())).unwrap(),
            DSM_BUCKET_NANOS,
            8,
            4096,
        )
        .unwrap();
        table.add_point(&DsmStatsPointInput {
            hash: 1,
            parent_hash: 0,
            edge_tags: &["direction:out", "topic:orders"],
            pathway_latency_ns: 1_000_000,
            edge_latency_ns: 1_000_000,
            payload_size_bytes: 100,
        });
        let state = DsmTableState {
            table,
            backlogs,
            key: key(),
            config: DsmConfig {
                endpoint: dsm_endpoint(&Endpoint::from_slice(&server.url("/"))).unwrap(),
                ..config()
            },
        };

        let drained = state.drain(true);
        assert_eq!(drained.stats.len(), 1);
        send_buckets(&NativeCapabilities::new_client(), &state, drained)
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[test]
    fn shm_paths_distinguish_tables() {
        let path = dsm_table_shm_path(&key());
        for other in [
            DsmTableKey {
                env: "env-b".into(),
                ..key()
            },
            DsmTableKey {
                version: "2.0".into(),
                ..key()
            },
            DsmTableKey {
                service: "service-b".into(),
                ..key()
            },
            DsmTableKey {
                primary_tag: "region:eu".into(),
                ..key()
            },
        ] {
            assert_ne!(path, dsm_table_shm_path(&other));
            assert_ne!(dsm_backlog_shm_path(&key()), dsm_backlog_shm_path(&other));
        }
        assert_ne!(path, dsm_backlog_shm_path(&key()));
        assert_ne!(
            path,
            crate::service::stats_flusher::env_stats_shm_path("env-a", "1.0", "service-a")
        );
    }

    #[test]
    fn the_endpoint_is_the_agents_pipeline_stats_path() {
        let agent = Endpoint::from_slice("http://localhost:8126");
        let endpoint = dsm_endpoint(&agent).unwrap();
        assert_eq!(
            endpoint.url.to_string(),
            "http://localhost:8126/v0.1/pipeline_stats"
        );

        let agentless = Endpoint {
            api_key: Some("key".into()),
            ..agent
        };
        assert!(dsm_endpoint(&agentless).is_none());
    }

    #[test]
    fn draining_waits_for_the_next_boundary() {
        let at = |nanos: u64| UNIX_EPOCH + Duration::from_nanos(nanos);
        // A microsecond rather than a nanosecond past the boundary: Windows keeps SystemTime in
        // 100 ns ticks, which would round a single nanosecond away.
        assert_eq!(
            until_next_drain(at(10 * DSM_BUCKET_NANOS + 1_000)),
            Duration::from_nanos(DSM_BUCKET_NANOS - 1_000) + DRAIN_GRACE
        );
        assert_eq!(
            until_next_drain(at(10 * DSM_BUCKET_NANOS)),
            Duration::from_nanos(DSM_BUCKET_NANOS) + DRAIN_GRACE
        );
    }

    /// Field names, types and nesting as the Java and Go tracers write them, with sketches as
    /// msgpack `bin`.
    #[test]
    fn the_payload_has_the_fields_the_backend_expects() {
        let stats = vec![DsmStatsBucket {
            start: 30_000_000_000,
            duration: DSM_BUCKET_NANOS,
            stats: vec![
                DsmStatsGroup {
                    hash: u64::MAX,
                    parent_hash: 7,
                    edge_tags: vec!["direction:out".into(), "topic:orders".into()],
                    pathway_latency: vec![1, 2],
                    edge_latency: vec![3],
                    payload_size: vec![],
                },
                DsmStatsGroup {
                    hash: 1,
                    parent_hash: 0,
                    edge_tags: vec![],
                    pathway_latency: vec![],
                    edge_latency: vec![],
                    payload_size: vec![],
                },
            ],
        }];
        let drained = Drained {
            stats,
            backlogs: vec![],
        };
        let bytes = encode_payload(&key(), &config(), &drained).unwrap();
        let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();

        let field = |v: &rmpv::Value, name: &str| -> rmpv::Value {
            v.as_map()
                .unwrap()
                .iter()
                .find(|(k, _)| k.as_str() == Some(name))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        let has = |v: &rmpv::Value, name: &str| {
            v.as_map()
                .unwrap()
                .iter()
                .any(|(k, _)| k.as_str() == Some(name))
        };

        assert_eq!(field(&value, "Env").as_str(), Some("env-a"));
        assert_eq!(field(&value, "Service").as_str(), Some("service-a"));
        assert_eq!(field(&value, "Version").as_str(), Some("1.0"));
        assert_eq!(field(&value, "Lang").as_str(), Some("php"));
        assert_eq!(field(&value, "TracerVersion").as_str(), Some("1.0.0"));
        assert_eq!(field(&value, "PrimaryTag").as_str(), Some(""));
        assert_eq!(field(&value, "ProductMask").as_u64(), Some(3));
        assert_eq!(
            field(&value, "ProcessTags").as_array().unwrap()[0].as_str(),
            Some("entrypoint.name:php")
        );

        let buckets = field(&value, "Stats");
        let bucket = &buckets.as_array().unwrap()[0];
        assert_eq!(field(bucket, "Start").as_u64(), Some(30_000_000_000));
        assert_eq!(field(bucket, "Duration").as_u64(), Some(DSM_BUCKET_NANOS));

        let points = field(bucket, "Stats");
        let points = points.as_array().unwrap();
        let first = &points[0];
        assert_eq!(field(first, "Hash").as_u64(), Some(u64::MAX));
        assert_eq!(field(first, "ParentHash").as_u64(), Some(7));
        let tags = field(first, "EdgeTags");
        let tags: Vec<_> = tags
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str())
            .collect();
        assert_eq!(tags, [Some("direction:out"), Some("topic:orders")]);
        assert_eq!(
            field(first, "PathwayLatency").as_slice(),
            Some(&[1u8, 2][..])
        );
        assert_eq!(field(first, "EdgeLatency").as_slice(), Some(&[3u8][..]));
        assert_eq!(field(first, "PayloadSize").as_slice(), Some(&[][..]));

        // As in the Java tracer, a point without edge tags carries no `EdgeTags` at all.
        assert!(!has(&points[1], "EdgeTags"));
        // Nor does a bucket without backlogs carry `Backlogs`.
        assert!(!has(bucket, "Backlogs"));
    }

    /// Backlogs go out in the bucket of the interval they were recorded in, next to its stats;
    /// an interval with only backlogs still gets a bucket, and buckets are ordered by start.
    #[test]
    fn backlogs_share_their_intervals_bucket_with_its_stats() {
        let group = || DsmStatsGroup {
            hash: 1,
            parent_hash: 0,
            edge_tags: vec![],
            pathway_latency: vec![],
            edge_latency: vec![],
            payload_size: vec![],
        };
        let backlog = |partition: &str, value| DsmBacklog {
            tags: vec![
                "topic:orders".into(),
                "type:kafka_produce".into(),
                format!("partition:{partition}"),
            ],
            value,
        };
        let drained = Drained {
            stats: vec![DsmStatsBucket {
                start: 2 * DSM_BUCKET_NANOS,
                duration: DSM_BUCKET_NANOS,
                stats: vec![group()],
            }],
            backlogs: vec![
                DsmBacklogBucket {
                    start: 2 * DSM_BUCKET_NANOS,
                    duration: DSM_BUCKET_NANOS,
                    backlogs: vec![backlog("0", 41)],
                },
                DsmBacklogBucket {
                    start: DSM_BUCKET_NANOS,
                    duration: DSM_BUCKET_NANOS,
                    backlogs: vec![backlog("1", 7)],
                },
            ],
        };
        let bytes = encode_payload(&key(), &config(), &drained).unwrap();
        let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
        let field = |v: &rmpv::Value, name: &str| -> rmpv::Value {
            v.as_map()
                .unwrap()
                .iter()
                .find(|(k, _)| k.as_str() == Some(name))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("missing {name}"))
        };

        let buckets = field(&value, "Stats");
        let buckets = buckets.as_array().unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(field(&buckets[0], "Start").as_u64(), Some(DSM_BUCKET_NANOS));
        assert!(field(&buckets[0], "Stats").as_array().unwrap().is_empty());
        assert_eq!(
            field(&buckets[1], "Start").as_u64(),
            Some(2 * DSM_BUCKET_NANOS)
        );
        assert_eq!(field(&buckets[1], "Stats").as_array().unwrap().len(), 1);

        let backlogs = field(&buckets[1], "Backlogs");
        let backlog = &backlogs.as_array().unwrap()[0];
        let tags: Vec<_> = field(backlog, "Tags")
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(tags, ["topic:orders", "type:kafka_produce", "partition:0"]);
        assert_eq!(field(backlog, "Value").as_i64(), Some(41));
    }
}
