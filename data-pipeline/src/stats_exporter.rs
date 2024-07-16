// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    ops::DerefMut,
    sync::{
        atomic::{self, AtomicU64},
        Mutex,
    },
    time,
};

use datadog_trace_normalization::normalize_utils;
use datadog_trace_protobuf::pb;
use ddcommon::{connector, tag::Tag, Endpoint};
use hyper;

const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";

/// The stats saved in the trace exporter are aggregated by BucketKey
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
struct AggregationKey {
    resource_name: String,
    service_name: String,
    operation_name: String,
    span_type: String,
    http_status_code: u32,
    is_synthetics_request: bool,
}

/// The stats stored for each BucketKey
#[derive(Debug, Default)]
struct GroupedStats {
    hits: u64,
    errors: u64,
    duration: u64,
    top_level_hits: u64,
    ok_summary: datadog_ddsketch::DDSketch,
    error_summary: datadog_ddsketch::DDSketch,
}

impl GroupedStats {
    fn insert(&mut self, span_stats: &SpanStats) {
        self.duration += span_stats.duration;
        self.hits += 1;

        if span_stats.is_error {
            self.errors += 1;
            let _ = self.error_summary.add(span_stats.duration as f64);
        } else {
            let _ = self.ok_summary.add(span_stats.duration as f64);
        }
        if span_stats.is_top_level {
            self.top_level_hits += 1;
        }
    }
}

/// Metadata required in a ClientStatsPayload
#[derive(Debug, Default, Clone)]
pub struct LibraryMetadata {
    pub hostname: String,
    pub env: String,
    pub version: String,
    pub lang: String,
    pub tracer_version: String,
    pub runtime_id: String,
    pub service: String,
    pub container_id: String,
    pub git_commit_sha: String,
    pub tags: Vec<Tag>,
}

/// Description of a span with only data required for stats
#[derive(Debug, Clone)]
pub struct SpanStats {
    pub resource_name: String,
    pub service_name: String,
    pub operation_name: String,
    pub span_type: String,
    pub http_status_code: u32,
    pub is_synthetics_request: bool,
    pub is_top_level: bool,
    pub is_error: bool,
    /// in nanoseconds
    pub duration: u64,
}

#[derive(Debug)]
struct StatsBucket {
    data: HashMap<AggregationKey, GroupedStats>,
    start: time::SystemTime,
}

impl StatsBucket {
    fn new() -> Self {
        Self {
            data: HashMap::new(),
            start: time::SystemTime::now(),
        }
    }
}

/// Stats exporter configuration
#[derive(Debug, Default)]
pub struct Configuration {
    /// time range of each bucket
    pub buckets_duration: time::Duration,
    /// optional timeout for sending stats
    pub request_timeout: Option<time::Duration>,
    /// endpoint used to send stats to the agent
    pub endpoint: Endpoint,
}

/// An exporter aggregating stats from traces and sending them to the agent
///
/// Currently we only keep one time bucket starting at the time of the exporter creation and
/// resetting to current time on flush. All `SpanStats` sent between flushesare added to the same
/// bucket.
/// This raises two issues:
/// - We expect SpanStats to be submitted right after the span ended (since the aggregation is done
///   on endTime)
/// - We expect the tracer to call send when we reach start_time + bucket_duration to make sure the
///   bucket is the correct size
#[derive(Debug)]
pub struct StatsExporter {
    buckets: Mutex<StatsBucket>,
    meta: LibraryMetadata,
    sequence_id: AtomicU64,
    client: ddcommon::HttpClient,
    cfg: Configuration,
}

impl StatsExporter {
    /// Return a new StatsExporter
    ///
    /// - `meta` is used when sending the ClientStatsPayload to the agent
    /// - `cfg` is the configuration for the stats exporter
    ///
    /// Returns a result to have the same signature as the blocking implementation.
    pub fn new(meta: LibraryMetadata, cfg: Configuration) -> anyhow::Result<Self> {
        Ok(Self {
            buckets: Mutex::new(StatsBucket::new()),
            meta,
            sequence_id: AtomicU64::new(0),
            client: hyper::Client::builder().build(connector::Connector::default()),
            cfg,
        })
    }

    /// Insert a new SpanStats into the corresponding bucket
    pub fn insert(&self, mut span_stat: SpanStats) {
        normalize_span_stat(&mut span_stat);
        obfuscate_span_stat(&mut span_stat);

        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets
            .data
            .entry(AggregationKey {
                resource_name: std::mem::take(&mut span_stat.resource_name),
                service_name: std::mem::take(&mut span_stat.service_name),
                operation_name: std::mem::take(&mut span_stat.operation_name),
                span_type: std::mem::take(&mut span_stat.span_type),
                http_status_code: span_stat.http_status_code,
                is_synthetics_request: span_stat.is_synthetics_request,
            })
            .or_default();

        bucket.insert(&span_stat);
    }

    /// Send the stats stored in the exporter and flush them
    pub async fn send(&self) -> anyhow::Result<()> {
        let payload = self.flush();
        let body = rmp_serde::encode::to_vec_named(&payload)?;
        let req = self
            .cfg
            .endpoint
            .into_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))
            .unwrap()
            .header(
                hyper::header::CONTENT_TYPE,
                ddcommon::header::APPLICATION_MSGPACK,
            )
            .method(hyper::Method::POST)
            .body(hyper::Body::from(body))?;

        let resp = match self.cfg.request_timeout {
            Some(t) => tokio::time::timeout(t, self.client.request(req)).await?,
            None => self.client.request(req).await,
        }?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "received {} status code from the agent",
                resp.status().as_u16()
            );
        }
        Ok(())
    }

    /// Flush all stats buckets into a payload
    fn flush(&self) -> pb::ClientStatsPayload {
        let sequence = self.sequence_id.fetch_add(1, atomic::Ordering::Relaxed);
        encode_stats_payload(
            self.meta.clone(),
            sequence,
            std::mem::replace(self.buckets.lock().unwrap().deref_mut(), StatsBucket::new()),
            self.cfg.buckets_duration,
        )
    }
}

fn normalize_span_stat(span: &mut SpanStats) {
    normalize_utils::normalize_service(&mut span.service_name);
    normalize_utils::normalize_name(&mut span.operation_name);
    normalize_utils::normalize_span_type(&mut span.span_type);
    normalize_utils::normalize_resource(&mut span.resource_name, &span.operation_name);
}

fn obfuscate_span_stat(span: &mut SpanStats) {
    match &*span.span_type {
        "redis" => {
            span.resource_name =
                datadog_trace_obfuscation::redis::obfuscate_redis_string(&span.resource_name);
        }
        "sql" | "cassandra" => {
            // TODO: integrate SQL obfuscation
        }
        _ => {}
    };
}

fn encode_bucket(key: AggregationKey, bucket: GroupedStats) -> pb::ClientGroupedStats {
    pb::ClientGroupedStats {
        service: key.service_name,
        name: key.operation_name,
        resource: key.resource_name,
        r#type: key.span_type,
        http_status_code: key.http_status_code,
        synthetics: key.is_synthetics_request,

        hits: bucket.hits,
        errors: bucket.errors,
        duration: bucket.duration,
        top_level_hits: bucket.top_level_hits,

        ok_summary: bucket.ok_summary.encode_to_vec(),
        error_summary: bucket.error_summary.encode_to_vec(),

        // TODO: this is not used in dotnet's stat computations
        // but is in the agent
        span_kind: String::new(),
        db_type: String::new(),
        peer_tags: Vec::new(),
        is_trace_root: pb::Trilean::False.into(),
    }
}

fn encode_stats_payload(
    meta: LibraryMetadata,
    sequence: u64,
    mut buckets: StatsBucket,
    stats_computation_interval: time::Duration,
) -> pb::ClientStatsPayload {
    pb::ClientStatsPayload {
        hostname: meta.hostname,
        env: meta.env,
        lang: meta.lang,
        version: meta.version,
        runtime_id: meta.runtime_id,
        tracer_version: meta.tracer_version,
        service: meta.service,
        container_id: meta.container_id,
        git_commit_sha: meta.git_commit_sha,
        tags: meta.tags.into_iter().map(|t| t.into_string()).collect(),

        sequence,

        stats: vec![pb::ClientStatsBucket {
            start: duration_unix_timestamp(buckets.start).as_nanos() as u64,
            duration: stats_computation_interval.as_nanos() as u64,
            stats: buckets
                .data
                .drain()
                .map(|(k, b)| encode_bucket(k, b))
                .collect(),

            // Agent-only field
            agent_time_shift: 0,
        }],

        // Agent-only field
        agent_aggregation: String::new(),
        image_tag: String::new(),
    }
}

fn duration_unix_timestamp(t: time::SystemTime) -> time::Duration {
    match t.duration_since(time::SystemTime::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => time::Duration::ZERO,
    }
}

/// Return a Endpoint to send stats to the agent at `agent_url`
pub fn stats_url_from_agent_url(agent_url: hyper::Uri) -> anyhow::Result<hyper::Uri> {
    let mut parts = agent_url.into_parts();
    parts.path_and_query = Some(http::uri::PathAndQuery::from_static(STATS_ENDPOINT_PATH));
    Ok(hyper::Uri::from_parts(parts)?)
}

/// Provides a blocking implementation of StatsExporter
pub mod blocking {

    use crate::stats_exporter::{Configuration, LibraryMetadata, SpanStats};

    /// Blocking implementation of StatsExporter
    #[derive(Debug)]
    pub struct StatsExporter {
        inner: super::StatsExporter,
        rt: tokio::runtime::Runtime,
    }

    impl StatsExporter {
        /// Return a new stats exporter which blocks on sending
        pub fn new(meta: LibraryMetadata, cfg: Configuration) -> anyhow::Result<Self> {
            Ok(Self {
                inner: super::StatsExporter::new(meta, cfg)?,
                rt: tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?,
            })
        }

        /// Insert a new SpanStats into the corresponding bucket
        pub fn insert(&self, span_stat: SpanStats) {
            self.inner.insert(span_stat)
        }

        /// Send the stats stored in the exporter and flush them in a synchronous way
        pub fn send(&self) -> anyhow::Result<()> {
            self.rt.block_on(self.inner.send())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use httpmock::prelude::*;
    use httpmock::MockServer;

    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}

    /// Fails to compile if stats exporter is not Send and Sync
    #[test]
    fn test_stats_exporter_sync_send() {
        let _ = is_send::<StatsExporter>;
        let _ = is_sync::<StatsExporter>;
    }

    /// Fails to compile if blocking stats exporter is not Send and Sync
    #[test]
    fn test_blocking_stats_exporter_sync_send() {
        let _ = is_send::<blocking::StatsExporter>;
        let _ = is_sync::<blocking::StatsExporter>;
    }

    fn get_test_metadata() -> LibraryMetadata {
        LibraryMetadata {
            hostname: "libdatadog-test".into(),
            env: "test".into(),
            version: "0.0.0".into(),
            lang: "rust".into(),
            tracer_version: "0.0.0".into(),
            runtime_id: "e39d6d12-0752-489f-b488-cf80006c0378".into(),
            service: "stats_exporter_test".into(),
            ..Default::default()
        }
    }

    fn get_test_configuration() -> Configuration {
        Configuration {
            buckets_duration: time::Duration::from_secs(10),
            request_timeout: None,
            endpoint: Endpoint {
                url: stats_url_from_agent_url(
                    hyper::Uri::from_str("http://localhost:8136").unwrap(),
                )
                .unwrap(),
                ..Default::default()
            },
        }
    }

    fn get_test_span_stats() -> SpanStats {
        SpanStats {
            resource_name: "res_1".into(),
            service_name: "service_a".into(),
            operation_name: "insert_stats".into(),
            span_type: "".into(),
            http_status_code: 200,
            is_synthetics_request: false,
            is_error: false,
            is_top_level: true,
            duration: 1_000_000_000,
        }
    }

    fn get_test_aggregation_key() -> AggregationKey {
        AggregationKey {
            resource_name: "res_1".into(),
            service_name: "service_a".into(),
            operation_name: "insert_stats".into(),
            span_type: "".into(),
            http_status_code: 200,
            is_synthetics_request: false,
        }
    }

    #[test]
    fn test_stats_computation() {
        let stats_exporter =
            StatsExporter::new(get_test_metadata(), get_test_configuration()).unwrap();

        let span_stats = get_test_span_stats();

        let span_aggregation_key = get_test_aggregation_key();

        for i in 0..100 {
            let mut s = span_stats.clone();
            s.duration += 10_000_000 * i;
            stats_exporter.insert(s)
        }

        for i in 0..100 {
            let mut s = span_stats.clone();
            s.is_error = true;
            s.duration += 20_000_000 * i;
            stats_exporter.insert(s)
        }

        let bucket = stats_exporter.buckets.lock().unwrap();
        let stats = bucket.data.get(&span_aggregation_key).unwrap();
        assert_eq!(stats.hits, 200);
        assert_eq!(stats.top_level_hits, 200);
        assert_eq!(
            stats.duration,
            1_000_000_000 * 200 + 10_000_000 * 4950 + 20_000_000 * 4950
        );
        assert_eq!(stats.errors, 100);
    }

    #[test]
    fn test_aggregation() {
        let stats_exporter =
            StatsExporter::new(get_test_metadata(), get_test_configuration()).unwrap();
        let span_stats = get_test_span_stats();
        let span_aggregation_key = get_test_aggregation_key();

        // Default Span
        for _ in 0..10 {
            let s = span_stats.clone();
            stats_exporter.insert(s)
        }

        // Default Span with error
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.is_error = true;
            s.is_top_level = false;
            stats_exporter.insert(s)
        }

        // Different service
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.service_name = "service_b".into();
            stats_exporter.insert(s)
        }

        // Different resource
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.resource_name = "res_2".into();
            stats_exporter.insert(s)
        }

        // Different operation
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.operation_name = "insert_new_stats".into();
            stats_exporter.insert(s)
        }

        // Different span type
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.span_type = "type".into();
            stats_exporter.insert(s)
        }

        // Different status code
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.http_status_code = 201;
            stats_exporter.insert(s)
        }

        // Synthetics
        for _ in 0..10 {
            let mut s = span_stats.clone();
            s.is_synthetics_request = true;
            stats_exporter.insert(s)
        }

        let bucket = stats_exporter.buckets.lock().unwrap();
        let stats = bucket.data.get(&span_aggregation_key).unwrap();
        assert_eq!(stats.hits, 20);

        let mut service_aggregation_key = span_aggregation_key.clone();
        service_aggregation_key.service_name = "service_b".into();
        let stats = bucket.data.get(&service_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);

        let mut resource_aggregation_key = span_aggregation_key.clone();
        resource_aggregation_key.resource_name = "res_2".into();
        let stats = bucket.data.get(&resource_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);

        let mut operation_aggregation_key = span_aggregation_key.clone();
        operation_aggregation_key.operation_name = "insert_new_stats".into();
        let stats = bucket.data.get(&operation_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);

        let mut type_aggregation_key = span_aggregation_key.clone();
        type_aggregation_key.span_type = "type".into();
        let stats = bucket.data.get(&type_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);

        let mut status_aggregation_key = span_aggregation_key.clone();
        status_aggregation_key.http_status_code = 201;
        let stats = bucket.data.get(&status_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);

        let mut synthetics_aggregation_key = span_aggregation_key.clone();
        synthetics_aggregation_key.is_synthetics_request = true;
        let stats = bucket.data.get(&synthetics_aggregation_key).unwrap();
        assert_eq!(stats.hits, 10);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_send_stats() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/v0.6/stats")
                    .body_contains("libdatadog-test")
                    .body_contains("res_1");
                then.status(200).body("");
            })
            .await;

        let stats_exporter = StatsExporter::new(
            get_test_metadata(),
            Configuration {
                buckets_duration: time::Duration::from_secs(10),
                request_timeout: None,
                endpoint: Endpoint {
                    url: stats_url_from_agent_url(server.url("/").parse::<hyper::Uri>().unwrap())
                        .unwrap(),
                    ..Default::default()
                },
            },
        )
        .unwrap();

        for _ in 0..100 {
            stats_exporter.insert(get_test_span_stats());
        }

        let send_status = stats_exporter.send().await;
        send_status.unwrap();

        mock.assert_async().await;
    }

    #[test]
    fn test_send_stats_blocking() {}
}
