// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.
use datadog_trace_protobuf::pb;
use datadog_trace_utils::span::trace_utils;
use datadog_trace_utils::span::Span;
use datadog_trace_utils::span::SpanText;
use std::borrow::Borrow;
use std::borrow::Cow;
use std::collections::HashMap;

const TAG_STATUS_CODE: &str = "http.status_code";
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SPANKIND: &str = "span.kind";
const TAG_ORIGIN: &str = "_dd.origin";

/// This struct represent the key used to group spans together to compute stats.
#[derive(Debug, Hash, PartialEq, Eq, Clone, Default)]
pub(super) struct AggregationKey<'a> {
    resource_name: Cow<'a, str>,
    service_name: Cow<'a, str>,
    operation_name: Cow<'a, str>,
    span_type: Cow<'a, str>,
    span_kind: Cow<'a, str>,
    http_status_code: u32,
    is_synthetics_request: bool,
    peer_tags: Vec<(Cow<'a, str>, Cow<'a, str>)>,
    is_trace_root: bool,
    http_method: Cow<'a, str>,
    http_endpoint: Cow<'a, str>,
}

/// Common representation of AggregationKey used to compare AggregationKey with different lifetimes
/// field order must be the same as in AggregationKey, o/wise hashes will be different
#[derive(Clone, Hash, PartialEq, Eq)]
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

/// Trait used to define a common type (`dyn BorrowableAggregationKey`) for all AggregationKey
/// regardless of lifetime.
/// This allows an hashmap with `AggregationKey<'static>` keys to lookup an entry with a
/// `AggregationKey<'a>`.
/// This is required because the `get_mut` method of Hashmap requires an input type `Q` such that
/// the key type `K` implements `Borrow<Q>`. Since `AggregationKey<'static>` cannot implement
/// `Borrow<AggregationKey<'a>>` we use `dyn BorrowableAggregationKey` as a placeholder.
trait BorrowableAggregationKey {
    fn borrowed_aggregation_key(&self) -> BorrowedAggregationKey<'_>;
}

impl BorrowableAggregationKey for AggregationKey<'_> {
    fn borrowed_aggregation_key(&self) -> BorrowedAggregationKey<'_> {
        BorrowedAggregationKey {
            resource_name: self.resource_name.borrow(),
            service_name: self.service_name.borrow(),
            operation_name: self.operation_name.borrow(),
            span_type: self.span_type.borrow(),
            span_kind: self.span_kind.borrow(),
            http_status_code: self.http_status_code,
            is_synthetics_request: self.is_synthetics_request,
            peer_tags: self
                .peer_tags
                .iter()
                .map(|(tag, value)| (tag.borrow(), value.borrow()))
                .collect(),
            is_trace_root: self.is_trace_root,
            http_method: self.http_method.borrow(),
            http_endpoint: self.http_endpoint.borrow(),
        }
    }
}

impl BorrowableAggregationKey for BorrowedAggregationKey<'_> {
    fn borrowed_aggregation_key(&self) -> BorrowedAggregationKey<'_> {
        self.clone()
    }
}

impl<'a, 'b> Borrow<dyn BorrowableAggregationKey + 'b> for AggregationKey<'a>
where
    'a: 'b,
{
    fn borrow(&self) -> &(dyn BorrowableAggregationKey + 'b) {
        self
    }
}

impl Eq for dyn BorrowableAggregationKey + '_ {}

impl PartialEq for dyn BorrowableAggregationKey + '_ {
    fn eq(&self, other: &dyn BorrowableAggregationKey) -> bool {
        self.borrowed_aggregation_key()
            .eq(&other.borrowed_aggregation_key())
    }
}

impl std::hash::Hash for dyn BorrowableAggregationKey + '_ {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.borrowed_aggregation_key().hash(state)
    }
}

impl<'a> AggregationKey<'a> {
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
            get_peer_tags(span, peer_tag_keys)
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

        Self {
            resource_name: span.resource.borrow().into(),
            service_name: span.service.borrow().into(),
            operation_name: span.name.borrow().into(),
            span_type: span.r#type.borrow().into(),
            span_kind: span_kind.into(),
            http_status_code: get_status_code(span),
            http_method: http_method.into(),
            http_endpoint: http_endpoint.into(),
            is_synthetics_request: span
                .meta
                .get(TAG_ORIGIN)
                .is_some_and(|origin| origin.borrow().starts_with(TAG_SYNTHETICS)),
            is_trace_root: span.parent_id == 0,
            peer_tags: peer_tags
                .into_iter()
                .map(|(k, v)| (k.into(), v.borrow().into()))
                .collect(),
        }
    }

    /// Clone the fields of an AggregationKey to produce a static version of the key which is
    /// not tied to the lifetime of a span.
    pub(super) fn into_static_key(self) -> AggregationKey<'static> {
        AggregationKey {
            resource_name: Cow::Owned(self.resource_name.into_owned()),
            service_name: Cow::Owned(self.service_name.into_owned()),
            operation_name: Cow::Owned(self.operation_name.into_owned()),
            span_type: Cow::Owned(self.span_type.into_owned()),
            span_kind: Cow::Owned(self.span_kind.into_owned()),
            http_status_code: self.http_status_code,
            http_method: Cow::Owned(self.http_method.into_owned()),
            http_endpoint: Cow::Owned(self.http_endpoint.into_owned()),
            is_synthetics_request: self.is_synthetics_request,
            is_trace_root: self.is_trace_root,
            peer_tags: self
                .peer_tags
                .into_iter()
                .map(|(key, value)| (Cow::from(key.into_owned()), Cow::from(value.into_owned())))
                .collect(),
        }
    }
}

impl From<pb::ClientGroupedStats> for AggregationKey<'static> {
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
                .filter_map(|t| {
                    let (key, value) = t.split_once(':')?;
                    Some((key.to_string().into(), value.to_string().into()))
                })
                .collect(),
            is_trace_root: value.is_trace_root == 1,
            http_method: value.http_method.into(),
            http_endpoint: value.http_endpoint.into(),
        }
    }
}

/// Return the status code of a span based on the metrics and meta tags.
fn get_status_code<T>(span: &Span<T>) -> u32
where
    T: SpanText,
{
    if let Some(status_code) = span.metrics.get(TAG_STATUS_CODE) {
        *status_code as u32
    } else if let Some(status_code) = span.meta.get(TAG_STATUS_CODE) {
        status_code.borrow().parse().unwrap_or(0)
    } else {
        0
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

/// Parse the meta tags of a span and return a list of the peer tags based on the list of
/// `peer_tag_keys`
fn get_peer_tags<'k, 'v, T>(span: &'v Span<T>, peer_tag_keys: &'k [String]) -> Vec<(&'k str, &'v T)>
where
    T: SpanText,
{
    peer_tag_keys
        .iter()
        .filter_map(|key| Some((key.as_str(), span.meta.get(key.as_str())?)))
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
    fn insert<T>(&mut self, value: &Span<T>)
    where
        T: SpanText,
    {
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
    data: HashMap<AggregationKey<'static>, GroupedStats>,
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
    pub(super) fn insert<T>(&mut self, key: AggregationKey<'_>, value: &Span<T>)
    where
        T: SpanText,
    {
        if let Some(grouped_stats) = self.data.get_mut(&key as &dyn BorrowableAggregationKey) {
            grouped_stats.insert(value);
        } else {
            let mut grouped_stats = GroupedStats::default();
            grouped_stats.insert(value);
            self.data.insert(key.into_static_key(), grouped_stats);
        }
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
        service: key.service_name.into_owned(),
        name: key.operation_name.into_owned(),
        resource: key.resource_name.into_owned(),
        http_status_code: key.http_status_code,
        r#type: key.span_type.into_owned(),
        db_type: String::new(), // db_type is not used yet (see proto definition)

        hits: group.hits,
        errors: group.errors,
        duration: group.duration,

        ok_summary: group.ok_summary.encode_to_vec(),
        error_summary: group.error_summary.encode_to_vec(),
        synthetics: key.is_synthetics_request,
        top_level_hits: group.top_level_hits,
        span_kind: key.span_kind.into_owned(),

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
        http_method: key.http_method.into_owned(),
        http_endpoint: key.http_endpoint.into_owned(),
        grpc_status_code: String::new(), // currently not used
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_utils::span::{SpanBytes, SpanSlice};

    use super::*;

    #[test]
    fn test_aggregation_key_from_span() {
        let test_cases: Vec<(SpanBytes, AggregationKey)> = vec![
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
                SpanBytes {
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
                SpanBytes {
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
                SpanBytes {
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
                SpanBytes {
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
                SpanBytes {
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
                AggregationKey {
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
                AggregationKey {
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

        let test_cases_with_peer_tags: Vec<(SpanSlice, AggregationKey)> = vec![
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
                AggregationKey {
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
                AggregationKey {
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
                AggregationKey::from_span(&span, test_peer_tags.as_slice()),
                expected_key
            );
        }
    }
}
