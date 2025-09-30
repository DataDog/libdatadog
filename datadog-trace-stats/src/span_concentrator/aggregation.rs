// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.
use datadog_trace_protobuf::pb;
use datadog_trace_utils::span::Span;
use datadog_trace_utils::span::SpanText;
use hashbrown::HashMap;

const TAG_STATUS_CODE: &str = "http.status_code";
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SPANKIND: &str = "span.kind";
const TAG_ORIGIN: &str = "_dd.origin";

#[derive(Clone, Hash, PartialEq, Eq)]
/// Represent a stats aggregation key borrowed from span data
pub(super) struct BorrowedAggregationKey<'a> {
    resource_name: &'a str,
    service_name: &'a str,
    operation_name: &'a str,
    span_type: &'a str,
    span_kind: &'a str,
    http_status_code: u32,
    is_synthetics_request: bool,
    peer_tags: Vec<(&'a str, &'a str)>,
    is_trace_root: bool,
    http_method: &'a str,
    http_endpoint: &'a str,
}

impl hashbrown::Equivalent<OwnedAggregationKey> for BorrowedAggregationKey<'_> {
    #[inline]
    fn equivalent(&self, key: &OwnedAggregationKey) -> bool {
        self.resource_name == key.resource_name
            && self.service_name == key.service_name
            && self.operation_name == key.operation_name
            && self.span_type == key.span_type
            && self.span_kind == key.span_kind
            && self.http_status_code == key.http_status_code
            && self.is_synthetics_request == key.is_synthetics_request
            && self.peer_tags.len() == key.peer_tags.len()
            && self
                .peer_tags
                .iter()
                .zip(key.peer_tags.iter())
                .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
            && self.is_trace_root == key.is_trace_root
            && self.http_method == key.http_method
            && self.http_endpoint == key.http_endpoint
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Default)]
/// Represents a span aggregation key with owned data
///
/// To be able to use BorrowedAggregationKey to index into a stats bucket hashmap two
/// conditions must stay true:
/// * Hashing an owned key derived from a borrowed key should produce the same hash as hashing the
///   borrowed key
/// * Running the Equivalent trait on an owned key derived from a borrowed key should produce true
pub(super) struct OwnedAggregationKey {
    resource_name: String,
    service_name: String,
    operation_name: String,
    span_type: String,
    span_kind: String,
    http_status_code: u32,
    is_synthetics_request: bool,
    peer_tags: Vec<(String, String)>,
    is_trace_root: bool,
    http_method: String,
    http_endpoint: String,
}

impl From<&BorrowedAggregationKey<'_>> for OwnedAggregationKey {
    fn from(value: &BorrowedAggregationKey<'_>) -> Self {
        OwnedAggregationKey {
            resource_name: value.resource_name.to_owned(),
            service_name: value.service_name.to_owned(),
            operation_name: value.operation_name.to_owned(),
            span_type: value.span_type.to_owned(),
            span_kind: value.span_kind.to_owned(),
            http_status_code: value.http_status_code,
            is_synthetics_request: value.is_synthetics_request,
            peer_tags: value
                .peer_tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            is_trace_root: value.is_trace_root,
            http_method: value.http_method.to_owned(),
            http_endpoint: value.http_endpoint.to_owned(),
        }
    }
}

impl<'a> BorrowedAggregationKey<'a> {
    /// Return an AggregationKey matching the given span.
    ///
    /// If `peer_tags_keys` is not empty then the peer tags of the span will be included in the
    /// key.
    pub(super) fn from_span<T>(span: &'a Span<T>, peer_tag_keys: &'a [String]) -> Self
    where
        T: SpanText,
    {
        let span_kind = span
            .meta
            .get(TAG_SPANKIND)
            .map(|s| s.borrow())
            .unwrap_or_default();
        let peer_tags = if client_or_producer(span_kind) {
            // Parse the meta tags of the span and return a list of the peer tags based on the list
            // of `peer_tag_keys`
            peer_tag_keys
                .iter()
                .filter_map(|key| Some(((key.as_str()), (span.meta.get(key.as_str())?.borrow()))))
                .collect()
        } else {
            vec![]
        };

        let http_method = span
            .meta
            .get("http.method")
            .map(|s| s.borrow())
            .unwrap_or_default();

        let http_endpoint = span
            .meta
            .get("http.endpoint")
            .or_else(|| span.meta.get("http.route"))
            .map(|s| s.borrow())
            .unwrap_or_default();

        let status_code = if let Some(status_code) = span.metrics.get(TAG_STATUS_CODE) {
            *status_code as u32
        } else if let Some(status_code) = span.meta.get(TAG_STATUS_CODE) {
            status_code.borrow().parse().unwrap_or(0)
        } else {
            0
        };

        Self {
            resource_name: span.resource.borrow(),
            service_name: span.service.borrow(),
            operation_name: span.name.borrow(),
            span_type: span.r#type.borrow(),
            span_kind,
            http_status_code: status_code,
            is_synthetics_request: span
                .meta
                .get(TAG_ORIGIN)
                .is_some_and(|origin| origin.borrow().starts_with(TAG_SYNTHETICS)),
            peer_tags,
            is_trace_root: span.parent_id == 0,
            http_method,
            http_endpoint,
        }
    }

    /// Return an AggregationKey matching the given span.
    ///
    /// If `peer_tags_keys` is not empty then the peer tags of the span will be included in the
    /// key.
    pub(super) fn from_pb_span(span: &'a pb::Span, peer_tag_keys: &'a [String]) -> Self {
        let span_kind = span
            .meta
            .get(TAG_SPANKIND)
            .map(|s| s.as_str())
            .unwrap_or("");

        let peer_tags = if client_or_producer(span_kind) {
            // Parse the meta tags of the span and return a list of the peer tags based on the list
            // of `peer_tag_keys`
            peer_tag_keys
                .iter()
                .filter_map(|key| Some(((key.as_str()), (span.meta.get(key)?.as_str()))))
                .collect()
        } else {
            vec![]
        };

        let http_method = span
            .meta
            .get("http.method")
            .map(|s| s.as_str())
            .unwrap_or_default();

        let http_endpoint = span
            .meta
            .get("http.endpoint")
            .or_else(|| span.meta.get("http.route"))
            .map(|s| s.as_str())
            .unwrap_or_default();

        let status_code = if let Some(status_code) = span.metrics.get(TAG_STATUS_CODE) {
            *status_code as u32
        } else if let Some(status_code) = span.meta.get(TAG_STATUS_CODE) {
            status_code.as_str().parse().unwrap_or(0)
        } else {
            0
        };

        Self {
            resource_name: span.resource.as_str(),
            service_name: span.service.as_str(),
            operation_name: span.name.as_str(),
            span_type: span.r#type.as_str(),
            span_kind,
            http_status_code: status_code,
            is_synthetics_request: span
                .meta
                .get(TAG_ORIGIN)
                .is_some_and(|origin| origin.as_str().starts_with(TAG_SYNTHETICS)),
            peer_tags,
            is_trace_root: span.parent_id == 0,
            http_method,
            http_endpoint,
        }
    }
}

impl From<pb::ClientGroupedStats> for OwnedAggregationKey {
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
                .filter_map(|t| {
                    let (key, value) = t.split_once(':')?;
                    Some((key.to_string(), value.to_string()))
                })
                .collect(),
            is_trace_root: value.is_trace_root == 1,
            http_method: value.http_method,
            http_endpoint: value.http_endpoint,
        }
    }
}

/// Return true if the span kind is "client" or "producer"
fn client_or_producer<T>(span_kind: T) -> bool
where
    T: SpanText,
{
    matches!(
        span_kind.borrow().to_lowercase().as_str(),
        "client" | "producer"
    )
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
    fn insert(&mut self, duration: i64, is_error: bool, is_top_level: bool) {
        self.hits += 1;
        self.duration += duration as u64;

        if is_error {
            self.errors += 1;
            let _ = self.error_summary.add(duration as f64);
        } else {
            let _ = self.ok_summary.add(duration as f64);
        }
        if is_top_level {
            self.top_level_hits += 1;
        }
    }
}

/// A time bucket used for stats aggregation. It stores a map of GroupedStats storing the stats of
/// spans aggregated on their AggregationKey.
#[derive(Debug, Clone)]
pub(super) struct StatsBucket {
    data: HashMap<OwnedAggregationKey, GroupedStats>,
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
    pub(super) fn insert(
        &mut self,
        key: BorrowedAggregationKey<'_>,
        duration: i64,
        is_error: bool,
        is_top_level: bool,
    ) {
        self.data
            .entry_ref(&key)
            .or_default()
            .insert(duration, is_error, is_top_level);
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
fn encode_grouped_stats(key: OwnedAggregationKey, group: GroupedStats) -> pb::ClientGroupedStats {
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

        peer_tags: key
            .peer_tags
            .into_iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect(),
        is_trace_root: if key.is_trace_root {
            pb::Trilean::True.into()
        } else {
            pb::Trilean::False.into()
        },
        http_method: key.http_method,
        http_endpoint: key.http_endpoint,
        grpc_status_code: String::new(), // currently not used
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_utils::span::{SpanBytes, SpanSlice};

    use super::*;
    use std::{collections::HashMap, hash::Hash};

    fn get_hash(v: &impl Hash) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::hash::DefaultHasher::new();
        v.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn test_aggregation_key_from_span() {
        let test_cases: Vec<(SpanBytes, OwnedAggregationKey)> = vec![
            // Root span
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with span kind
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("span.kind".into(), "client".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
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
                SpanBytes {
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
                OwnedAggregationKey {
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
                SpanBytes {
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
                OwnedAggregationKey {
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
                SpanBytes {
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
                OwnedAggregationKey {
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
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("_dd.origin".into(), "synthetics-browser".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
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
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".into(), "418".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
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
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("http.status_code".into(), "x".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
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
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: HashMap::from([("http.status_code".into(), 418.0)]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    http_status_code: 418,
                    ..Default::default()
                },
            ),
            // Span with http.method and http.route
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "GET /api/v1/users".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("http.method".into(), "GET".into()),
                        ("http.route".into(), "/api/v1/users".into()),
                    ]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "GET /api/v1/users".into(),
                    http_method: "GET".into(),
                    http_endpoint: "/api/v1/users".into(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with http.method and http.endpoint (http.endpoint takes precedence)
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "POST /users/create".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("http.method".into(), "POST".into()),
                        ("http.route".into(), "/users/create".into()),
                        ("http.endpoint".into(), "/users/create2".into()),
                    ]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "POST /users/create".into(),
                    http_method: "POST".into(),
                    http_endpoint: "/users/create2".into(),
                    is_synthetics_request: false,
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
        ];

        let test_peer_tags = vec![
            "aws.s3.bucket".to_string(),
            "db.instance".to_string(),
            "db.system".to_string(),
        ];

        let test_cases_with_peer_tags: Vec<(SpanSlice, OwnedAggregationKey)> = vec![
            // Span with peer tags with peertags aggregation enabled
            (
                SpanSlice {
                    service: "service",
                    name: "op",
                    resource: "res",
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("span.kind", "client"), ("aws.s3.bucket", "bucket-a")]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: true,
                    peer_tags: vec![("aws.s3.bucket".into(), "bucket-a".into())],
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags with peertags aggregation enabled
            (
                SpanSlice {
                    service: "service",
                    name: "op",
                    resource: "res",
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind", "producer"),
                        ("aws.s3.bucket", "bucket-a"),
                        ("db.instance", "dynamo.test.us1"),
                        ("db.system", "dynamodb"),
                    ]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "producer".into(),
                    peer_tags: vec![
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ],
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with multiple peer tags with peertags aggregation enabled and span kind is
            // server
            (
                SpanSlice {
                    service: "service",
                    name: "op",
                    resource: "res",
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([
                        ("span.kind", "server"),
                        ("aws.s3.bucket", "bucket-a"),
                        ("db.instance", "dynamo.test.us1"),
                        ("db.system", "dynamodb"),
                    ]),
                    ..Default::default()
                },
                OwnedAggregationKey {
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
            let borrowed_key = BorrowedAggregationKey::from_span(&span, &[]);
            assert_eq!(OwnedAggregationKey::from(&borrowed_key), expected_key);
            assert_eq!(
                get_hash(&borrowed_key),
                get_hash(&OwnedAggregationKey::from(&borrowed_key))
            );
        }

        for (span, expected_key) in test_cases_with_peer_tags {
            let borrowed_key = BorrowedAggregationKey::from_span(&span, test_peer_tags.as_slice());
            assert_eq!(OwnedAggregationKey::from(&borrowed_key), expected_key);
            assert_eq!(
                get_hash(&borrowed_key),
                get_hash(&OwnedAggregationKey::from(&borrowed_key))
            );
        }
    }
}
