use std::{
    collections::HashMap,
    sync::{
        atomic::{self, AtomicU64},
        Mutex,
    },
};

use anyhow::Ok;
use datadog_trace_normalization::normalize_utils;
use datadog_trace_protobuf::pb;
use ddcommon::{connector, tag::Tag, Endpoint};
use hyper::{Method, Uri};

#[derive(Debug, Hash, PartialEq, Eq)]
struct BucketKey {
    resource_name: String,
    service_name: String,
    operation_name: String,
    span_type: String,
    http_status_code: u32,
    is_synthetics_request: bool,
}

#[derive(Debug, Default)]
struct Bucket {
    hits: u64,
    errors: u64,
    bucket_duration: u64,
    top_level_hits: u64,
    ok_summary: datadog_ddsketch::DDSketch,
    error_summary: datadog_ddsketch::DDSketch,
}

fn encode_bucket(key: BucketKey, bucket: Bucket) -> pb::ClientGroupedStats {
    pb::ClientGroupedStats {
        service: key.service_name,
        name: key.operation_name,
        resource: key.resource_name,
        r#type: key.span_type,
        http_status_code: key.http_status_code,
        synthetics: key.is_synthetics_request,

        hits: bucket.hits,
        errors: bucket.errors,
        duration: bucket.bucket_duration,
        top_level_hits: bucket.top_level_hits,

        ok_summary: bucket.ok_summary.encode_to_vec(),
        error_summary: bucket.error_summary.encode_to_vec(),

        // TODO this is not used in dotnet's stat computations
        // but is in the agent
        span_kind: String::new(),
        db_type: String::new(),
        peer_tags: Vec::new(),
    }
}

#[derive(Debug, Default)]
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

pub struct SpanStats {
    pub resource_name: String,
    pub service_name: String,
    pub operation_name: String,
    pub span_type: String,
    pub http_status_code: u32,
    pub is_synthetics_request: bool,
    pub is_top_level: bool,
    pub is_error: bool,
    pub duration: u64,
}

#[derive(Debug, Default)]
struct StatsBuckets {
    buckets: HashMap<BucketKey, Bucket>,
}

impl StatsBuckets {
    fn insert(&mut self, mut span_stat: SpanStats) {
        normalize_span_stat(&mut span_stat);

        let bucket = self
            .buckets
            .entry(BucketKey {
                resource_name: span_stat.resource_name,
                service_name: span_stat.service_name,
                operation_name: span_stat.operation_name,
                span_type: span_stat.span_type,
                http_status_code: span_stat.http_status_code,
                is_synthetics_request: span_stat.is_synthetics_request,
            })
            .or_default();

        bucket.bucket_duration += span_stat.duration;
        bucket.hits += 1;

        if span_stat.is_error {
            bucket.errors += 1;
            bucket.error_summary.add(span_stat.duration as f64);
        } else {
            bucket.ok_summary.add(span_stat.duration as f64);
        }
        if span_stat.is_top_level {
            bucket.top_level_hits += 1;
        }
    }
}

#[derive(Debug)]
pub struct StatsExporter {
    buckets: Mutex<StatsBuckets>,
    meta: LibraryMetadata,
    sequence_id: AtomicU64,

    rt: tokio::runtime::Runtime,
    client: ddcommon::HttpClient,
    endpoint: ddcommon::Endpoint,
}

impl StatsExporter {
    pub fn new(meta: LibraryMetadata, endpoint: ddcommon::Endpoint) -> anyhow::Result<Self> {
        Ok(Self {
            buckets: Mutex::default(),
            meta,
            sequence_id: AtomicU64::new(0),

            endpoint,

            // TODO return error
            rt: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            client: hyper::Client::builder().build(connector::Connector::default()),
        })
    }

    pub fn insert(&self, span_stat: SpanStats) {
        self.buckets.lock().unwrap().insert(span_stat)
    }

    pub fn send(&self) -> anyhow::Result<()> {
        let payload = self.flush();
        let body = rmp_serde::encode::to_vec(&payload)?;
        let req = self
            .endpoint
            .into_request_builder(concat!("Libdatadog/", env!("CARGO_PKG_VERSION")))
            .unwrap()
            .header(
                hyper::header::CONTENT_TYPE,
                ddcommon::header::APPLICATION_MSGPACK,
            )
            .method(Method::POST)
            .body(hyper::Body::from(body))?;

        self.rt.block_on(async { self.client.request(req).await })?;
        Ok(())
    }

    fn flush(&self) -> pb::ClientStatsPayload {
        let sequence = self.sequence_id.fetch_add(1, atomic::Ordering::Relaxed);
        pb::ClientStatsPayload {
            hostname: self.meta.hostname.clone(),
            env: self.meta.env.clone(),
            lang: self.meta.lang.clone(),
            version: self.meta.version.clone(),
            runtime_id: self.meta.runtime_id.clone(),
            tracer_version: self.meta.tracer_version.clone(),
            service: self.meta.service.clone(),
            container_id: self.meta.container_id.clone(),
            git_commit_sha: self.meta.git_commit_sha.clone(),
            tags: self.meta.tags.iter().map(|t| t.to_string()).collect(),

            sequence,

            stats: vec![pb::ClientStatsBucket {
                start: 0,
                duration: 0,
                stats: std::mem::take(&mut *self.buckets.lock().unwrap())
                    .buckets
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
}

fn normalize_span_stat(span: &mut SpanStats) {
    normalize_utils::normalize_service(&mut span.service_name);
    normalize_utils::normalize_name(&mut span.operation_name);
    normalize_utils::normalize_span_type(&mut span.span_type);
    normalize_utils::normalize_resource(&mut span.resource_name, &span.operation_name);
}

pub fn endpoint_from_agent_url(agent_url: Uri) -> anyhow::Result<Endpoint> {
    let mut parts = agent_url.into_parts();
    parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/v0.6/stats"));
    let url = hyper::Uri::from_parts(parts)?;
    Ok(Endpoint { url, api_key: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_send<T: Send>(_: T) {}
    fn is_sync<T: Sync>(_: T) {}

    #[test]
    fn test_handle_sync_send() {
        #[allow(clippy::redundant_closure)]
        let _ = |h: StatsExporter| is_send(h);
        #[allow(clippy::redundant_closure)]
        let _ = |h: StatsExporter| is_sync(h);
    }
}
