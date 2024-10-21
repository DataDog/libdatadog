// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::has_top_level;
use ddcommon::tag::Tag;
use std::collections::HashMap;

const TAG_STATUS_CODE: &str = "http.status_code";
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SPANKIND: &str = "span.kind";
const TAG_ORIGIN: &str = "_dd.origin";

/// This struct represent the key used to group spans together to compute stats.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Default)]
pub(super) struct AggregationKey {
    resource_name: String,
    service_name: String,
    operation_name: String,
    span_type: String,
    span_kind: String,
    http_status_code: u32,
    is_synthetics_request: bool,
    peer_tags: Vec<Tag>,
    is_trace_root: bool,
}

impl AggregationKey {
    /// Return an AggregationKey matching the given span.
    ///
    /// If `peer_tags_keys` is not empty then the peer tags of the span will be included in the
    /// key.
    pub(super) fn from_span(span: &pb::Span, peer_tag_keys: &[String]) -> Self {
        let span_kind = span
            .meta
            .get(TAG_SPANKIND)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let peer_tags = if client_or_producer(&span_kind) {
            get_peer_tags(span, peer_tag_keys)
        } else {
            vec![]
        };
        Self {
            resource_name: span.resource.clone(),
            service_name: span.service.clone(),
            operation_name: span.name.clone(),
            span_type: span.r#type.clone(),
            span_kind,
            http_status_code: get_status_code(span),
            is_synthetics_request: span
                .meta
                .get(TAG_ORIGIN)
                .is_some_and(|origin| origin.starts_with(TAG_SYNTHETICS)),
            is_trace_root: span.parent_id == 0,
            peer_tags,
        }
    }
}

impl From<pb::ClientGroupedStats> for AggregationKey {
    fn from(value: pb::ClientGroupedStats) -> Self {
        Self {
            resource_name: value.resource,
            service_name: value.service,
            operation_name: value.name,
            span_type: value.r#type,
            span_kind: value.span_kind,
            http_status_code: value.http_status_code,
            is_synthetics_request: value.synthetics,
            peer_tags: value
                .peer_tags
                .into_iter()
                .flat_map(|t| ddcommon::tag::parse_tags(&t).0)
                .collect(),
            is_trace_root: value.is_trace_root == 1,
        }
    }
}

/// Return the status code of a span based on the metrics and meta tags.
fn get_status_code(span: &pb::Span) -> u32 {
    if let Some(status_code) = span.metrics.get(TAG_STATUS_CODE) {
        *status_code as u32
    } else if let Some(status_code) = span.meta.get(TAG_STATUS_CODE) {
        status_code.parse().unwrap_or(0)
    } else {
        0
    }
}

/// Return true if the span kind is "client" or "producer"
fn client_or_producer(span_kind: &str) -> bool {
    matches!(span_kind.to_lowercase().as_str(), "client" | "producer")
}

/// Parse the meta tags of a span and return a list of the peer tags based on the list of
/// `peer_tag_keys`
fn get_peer_tags(span: &pb::Span, peer_tag_keys: &[String]) -> Vec<Tag> {
    peer_tag_keys
        .iter()
        .filter_map(|key| Tag::new(key, span.meta.get(key)?).ok())
        .collect()
}

/// The stats computed from a group of span with the same AggregationKey
#[derive(Debug, Default, Clone)]
pub(super) struct GroupedStats {
    hits: u64,
    errors: u64,
    duration: u64,
    top_level_hits: u64,
    ok_summary: datadog_ddsketch::DDSketch,
    error_summary: datadog_ddsketch::DDSketch,
}

impl GroupedStats {
    /// Update the stats of a GroupedStats by inserting a span.
    fn insert(&mut self, value: &pb::Span) {
        self.hits += 1;
        self.duration += value.duration as u64;

        if value.error != 0 {
            self.errors += 1;
            let _ = self.error_summary.add(value.duration as f64);
        } else {
            let _ = self.ok_summary.add(value.duration as f64);
        }
        if has_top_level(value) {
            self.top_level_hits += 1;
        }
    }
}

/// A time bucket used for stats aggregation. It stores a map of GroupedStats storing the stats of
/// spans aggregated on their AggregationKey.
#[derive(Debug, Clone)]
pub(super) struct StatsBucket {
    data: HashMap<AggregationKey, GroupedStats>,
    start: u64,
}

impl StatsBucket {
    /// Return a new StatsBucket starting at the given timestamp
    pub(super) fn new(start_timestamp: u64) -> Self {
        Self {
            data: HashMap::new(),
            start: start_timestamp,
        }
    }

    /// Insert a value as stats in the group corresponding to the aggregation key, if it does
    /// not exist it creates it.
    pub(super) fn insert(&mut self, key: AggregationKey, value: &pb::Span) {
        self.data.entry(key).or_default().insert(value);
    }

    /// Consume the bucket and return a ClientStatsBucket containing the bucket stats.
    /// `bucket_duration` is the size of buckets for the concentrator containing the bucket.
    pub(super) fn flush(self, bucket_duration: u64) -> pb::ClientStatsBucket {
        pb::ClientStatsBucket {
            start: self.start,
            duration: bucket_duration,
            stats: self
                .data
                .into_iter()
                .map(|(k, b)| encode_grouped_stats(k, b))
                .collect(),
            // Agent-only field
            agent_time_shift: 0,
        }
    }
}

/// Create a ClientGroupedStats struct based on the given AggregationKey and GroupedStats
fn encode_grouped_stats(key: AggregationKey, group: GroupedStats) -> pb::ClientGroupedStats {
    pb::ClientGroupedStats {
        service: key.service_name,
        name: key.operation_name,
        resource: key.resource_name,
        http_status_code: key.http_status_code,
        r#type: key.span_type,
        db_type: String::new(), // db_type is not used yet (see proto definition)

        hits: group.hits,
        errors: group.errors,
        duration: group.duration,

        ok_summary: group.ok_summary.encode_to_vec(),
        error_summary: group.error_summary.encode_to_vec(),
        synthetics: key.is_synthetics_request,
        top_level_hits: group.top_level_hits,
        span_kind: key.span_kind,

        peer_tags: key.peer_tags.into_iter().map(|t| t.to_string()).collect(),
        is_trace_root: if key.is_trace_root {
            pb::Trilean::True.into()
        } else {
            pb::Trilean::False.into()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddcommon::tag;

    #[test]
    fn test_aggregation_key_from_span() {
        let test_cases: Vec<(pb::Span, AggregationKey)> = vec![
            // Root span
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with span kind
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("span.kind".to_string(), "client".to_string())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "client".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with peer tags but peertags aggregation disabled
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "client".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "client".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags but peertags aggregation disabled
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "producer".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                        ("db.instance".to_string(), "dynamo.test.us1".to_string()),
                        ("db.system".to_string(), "dynamodb".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "producer".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags but peertags aggregation disabled and span kind is
            // server
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "server".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                        ("db.instance".to_string(), "dynamo.test.us1".to_string()),
                        ("db.system".to_string(), "dynamodb".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "server".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span from synthetics
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([(
                        "_dd.origin".to_string(),
                        "synthetics-browser".to_string(),
                    )]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    is_synthetics_request: true,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with status code in meta
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".to_string(), "418".to_string())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    http_status_code: 418,
                    ..Default::default()
                },
            ),
            // Span with invalid status code in meta
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".to_string(), "x".to_string())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with status code in metrics
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: HashMap::from([("http.status_code".to_string(), 418.0)]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    http_status_code: 418,
                    ..Default::default()
                },
            ),
        ];

        let test_peer_tags = vec![
            "aws.s3.bucket".to_string(),
            "db.instance".to_string(),
            "db.system".to_string(),
        ];

        let test_cases_with_peer_tags: Vec<(pb::Span, AggregationKey)> = vec![
            // Span with peer tags with peertags aggregation enabled
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "client".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "client".to_string(),
                    is_trace_root: true,
                    peer_tags: vec![tag!("aws.s3.bucket", "bucket-a")],
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags with peertags aggregation enabled
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "producer".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                        ("db.instance".to_string(), "dynamo.test.us1".to_string()),
                        ("db.system".to_string(), "dynamodb".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "producer".to_string(),
                    peer_tags: vec![
                        tag!("aws.s3.bucket", "bucket-a"),
                        tag!("db.instance", "dynamo.test.us1"),
                        tag!("db.system", "dynamodb"),
                    ],
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags with peertags aggregation enabled and span kind is
            // server
            (
                pb::Span {
                    service: "service".to_string(),
                    name: "op".to_string(),
                    resource: "res".to_string(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".to_string(), "server".to_string()),
                        ("aws.s3.bucket".to_string(), "bucket-a".to_string()),
                        ("db.instance".to_string(), "dynamo.test.us1".to_string()),
                        ("db.system".to_string(), "dynamodb".to_string()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".to_string(),
                    operation_name: "op".to_string(),
                    resource_name: "res".to_string(),
                    span_kind: "server".to_string(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
        ];

        for (span, expected_key) in test_cases {
            assert_eq!(AggregationKey::from_span(&span, &[]), expected_key);
        }

        for (span, expected_key) in test_cases_with_peer_tags {
            assert_eq!(
                AggregationKey::from_span(&span, &test_peer_tags),
                expected_key
            );
        }
    }
}
