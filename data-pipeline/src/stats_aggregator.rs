use std::collections::HashMap;

use datadog_trace_normalization::normalize_utils;
use datadog_trace_protobuf::pb;

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
struct StatsAggregator {
    buckets: HashMap<BucketKey, Bucket>,
    meta: LibraryMetadata,
    sequence_id: u64,
}

#[derive(Debug, Default)]
struct LibraryMetadata {
    hostname: String,
    env: String,
    version: String,
    lang: String,
    tracer_version: String,
    runtime_id: String,
    service: String,
    container_id: String,
    git_commit_sha: String,
    tags: Vec<String>,
}

struct SpanStat {
    resource_name: String,
    service_name: String,
    operation_name: String,
    span_type: String,
    http_status_code: u32,
    is_synthetics_request: bool,
    is_top_level: bool,
    is_error: bool,
    duration: u64,
}

impl StatsAggregator {
    pub fn new(meta: LibraryMetadata) -> Self {
        Self {
            buckets: HashMap::new(),
            meta,
            sequence_id: 0,
        }
    }

    pub fn insert(&mut self, mut span_stat: SpanStat) {
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

    fn send(&mut self) {
        todo!()
    }

    fn flush(&mut self) -> pb::ClientStatsPayload {
        self.sequence_id += 1;
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
            tags: self.meta.tags.clone(),

            sequence: self.sequence_id,

            stats: vec![pb::ClientStatsBucket {
                start: 0,
                duration: 0,
                stats: self
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

fn normalize_span_stat(span: &mut SpanStat) {
    normalize_utils::normalize_service(&mut span.service_name);
    normalize_utils::normalize_name(&mut span.operation_name);
    normalize_utils::normalize_span_type(&mut span.span_type);
    normalize_utils::normalize_resource(&mut span.resource_name, &span.operation_name);
}
