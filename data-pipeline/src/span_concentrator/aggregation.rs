// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.
use datadog_trace_protobuf::pb;
use datadog_trace_utils::span_v04::{trace_utils, Span};
use ddcommon::tag::Tag;
use std::collections::HashMap;
use tinybytes::BytesString;

const TAG_STATUS_CODE: &str = "http.status_code";
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SPANKIND: &str = "span.kind";
const TAG_ORIGIN: &str = "_dd.origin";

/// This struct represent the key used to group spans together to compute stats.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Default)]
pub(super) struct AggregationKey {
    resource_name: BytesString,
    service_name: BytesString,
    operation_name: BytesString,
    span_type: BytesString,
    span_kind: BytesString,
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
    pub(super) fn from_span(span: &Span, peer_tag_keys: &[String]) -> Self {
        let span_kind = span.meta.get(TAG_SPANKIND).cloned().unwrap_or_default();
        let peer_tags = if client_or_producer(span_kind.as_str()) {
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
                .is_some_and(|origin| origin.as_str().starts_with(TAG_SYNTHETICS)),
            is_trace_root: span.parent_id == 0,
            peer_tags,
        }
    }
}

impl From<pb::ClientGroupedStats> for AggregationKey {
    fn from(value: pb::ClientGroupedStats) -> Self {
        Self {
            resource_name: value.resource.into(),
            service_name: value.service.into(),
            operation_name: value.name.into(),
            span_type: value.r#type.into(),
            span_kind: value.span_kind.into(),
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
fn get_status_code(span: &Span) -> u32 {
    if let Some(status_code) = span.metrics.get(TAG_STATUS_CODE) {
        *status_code as u32
    } else if let Some(status_code) = span.meta.get(TAG_STATUS_CODE) {
        status_code.as_str().parse().unwrap_or(0)
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
fn get_peer_tags(span: &Span, peer_tag_keys: &[String]) -> Vec<Tag> {
    peer_tag_keys
        .iter()
        .filter_map(|key| Tag::new(key, span.meta.get(key.as_str()).as_ref()?).ok())
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
    fn insert(&mut self, value: &Span) {
        self.hits += 1;
        self.duration += value.duration as u64;

        if value.error != 0 {
            self.errors += 1;
            let _ = self.error_summary.add(value.duration as f64);
        } else {
            let _ = self.ok_summary.add(value.duration as f64);
        }
        if trace_utils::has_top_level(value) {
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
    pub(super) fn insert(&mut self, key: AggregationKey, value: &Span) {
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
        service: key.service_name.as_str().to_owned(),
        name: key.operation_name.as_str().to_owned(),
        resource: key.resource_name.as_str().to_owned(),
        http_status_code: key.http_status_code,
        r#type: key.span_type.as_str().to_owned(),
        db_type: String::new(), // db_type is not used yet (see proto definition)

        hits: group.hits,
        errors: group.errors,
        duration: group.duration,

        ok_summary: group.ok_summary.encode_to_vec(),
        error_summary: group.error_summary.encode_to_vec(),
        synthetics: key.is_synthetics_request,
        top_level_hits: group.top_level_hits,
        span_kind: key.span_kind.as_str().to_owned(),

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
        let test_cases: Vec<(Span, AggregationKey)> = vec![
            // Root span
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with span kind
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("span.kind".into(), "client".into())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with peer tags but peertags aggregation disabled
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "client".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags but peertags aggregation disabled
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "producer".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "producer".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags but peertags aggregation disabled and span kind is
            // server
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "server".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "server".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span from synthetics
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("_dd.origin".into(), "synthetics-browser".into())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: true,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with status code in meta
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".into(), "418".into())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    http_status_code: 418,
                    ..Default::default()
                },
            ),
            // Span with invalid status code in meta
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".into(), "x".into())]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with status code in metrics
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: HashMap::from([("http.status_code".into(), 418.0)]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
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

        let test_cases_with_peer_tags: Vec<(Span, AggregationKey)> = vec![
            // Span with peer tags with peertags aggregation enabled
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "client".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: true,
                    peer_tags: vec![tag!("aws.s3.bucket", "bucket-a")],
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags with peertags aggregation enabled
            (
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "producer".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "producer".into(),
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
                Span {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind".into(), "server".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]),
                    ..Default::default()
                },
                AggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "server".into(),
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
