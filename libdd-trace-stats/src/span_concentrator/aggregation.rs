// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.

use hashbrown::HashMap;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::SpanText;

use crate::span_concentrator::StatSpan;

const TAG_STATUS_CODE: &str = "http.status_code";
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SPANKIND: &str = "span.kind";
const TAG_ORIGIN: &str = "_dd.origin";
const TAG_SVC_SRC: &str = "_dd.svc_src";
const GRPC_STATUS_CODE_FIELD: &[&str] = &[
    "rpc.grpc.status_code",
    "grpc.code",
    "rpc.grpc.status.code",
    "grpc.status.code",
];

#[derive(Clone, Hash, PartialEq, Eq)]
/// Represent a stats aggregation key borrowed from span data
pub(super) struct BorrowedAggregationKey<'a> {
    resource_name: String,
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
    grpc_status_code: Option<u8>,
    service_source: &'a str,
}

impl hashbrown::Equivalent<OwnedAggregationKey> for BorrowedAggregationKey<'_> {
    #[inline]
    fn equivalent(
        &self,
        OwnedAggregationKey {
            resource_name,
            service_name,
            operation_name,
            span_type,
            span_kind,
            http_status_code,
            is_synthetics_request,
            peer_tags,
            is_trace_root,
            http_method,
            http_endpoint,
            grpc_status_code,
            service_source,
        }: &OwnedAggregationKey,
    ) -> bool {
        &self.resource_name == resource_name
            && self.service_name == service_name
            && self.operation_name == operation_name
            && self.span_type == span_type
            && self.span_kind == span_kind
            && self.http_status_code == *http_status_code
            && self.is_synthetics_request == *is_synthetics_request
            && self.peer_tags.len() == peer_tags.len()
            && self
                .peer_tags
                .iter()
                .zip(peer_tags.iter())
                .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
            && self.is_trace_root == *is_trace_root
            && self.http_method == http_method
            && self.http_endpoint == http_endpoint
            && self.grpc_status_code == *grpc_status_code
            && self.service_source == service_source
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
    grpc_status_code: Option<u8>,
    service_source: String,
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
            grpc_status_code: value.grpc_status_code,
            service_source: value.service_source.to_owned(),
        }
    }
}

fn float_to_int(f: f64) -> Option<u8> {
    if f.floor() != f {
        return None;
    }
    if f < 0.0 || (u8::MAX as f64) < f {
        return None;
    }
    Some(f as u8)
}

fn get_grpc_status_code<'a>(span: &'a impl StatSpan<'a>) -> Option<u8> {
    for key in GRPC_STATUS_CODE_FIELD {
        if let Some(val) = span.get_meta(key) {
            if let Some(code) = grpc_status_str_to_int_value(val) {
                return Some(code);
            }
        }
    }

    for key in GRPC_STATUS_CODE_FIELD {
        if let Some(val) = span.get_metrics(key) {
            if let Some(code) = float_to_int(val) {
                return Some(code);
            }
        }
    }

    None
}

fn grpc_status_str_to_int_value(v: &str) -> Option<u8> {
    if let Ok(status) = v.parse() {
        return Some(status);
    }
    let mut status_uppercase = [0u8; 32];
    let mut status = v.trim_start_matches("StatusCode.");

    let mut needs_upcasing = false;
    for b in status.as_bytes() {
        if !b.is_ascii() {
            return None;
        }
        needs_upcasing |= b.is_ascii_lowercase()
    }
    if needs_upcasing {
        for (c, d) in status.as_bytes().iter().zip(&mut status_uppercase) {
            *d = c.to_ascii_uppercase();
        }
        status = std::str::from_utf8(&status_uppercase[0..status.len().min(status_uppercase.len())])
            .ok()?
    }

    match status {
        "OK" => return Some(0),
        "CANCELLED" | "CANCELED" => return Some(1),
        "UNKNOWN" => return Some(2),
        "INVALID_ARGUMENT" | "INVALIDARGUMENT" => return Some(3),
        "DEADLINE_EXCEEDED" | "DEADLINEEXCEEDED" => return Some(4),
        "NOT_FOUND" | "NOTFOUND" => return Some(5),
        "ALREADY_EXISTS" | "ALREADYEXISTS" => return Some(6),
        "PERMISSION_DENIED" | "PERMISSIONDENIED" => return Some(7),
        "UNAUTHENTICATED" => return Some(16),
        "RESOURCE_EXHAUSTED" | "RESOURCEEXHAUSTED" => return Some(8),
        "FAILED_PRECONDITION" | "FAILEDPRECONDITION" => return Some(9),
        "ABORTED" => return Some(10),
        "OUT_OF_RANGE" | "OUTOFRANGE" => return Some(11),
        "UNIMPLEMENTED" => return Some(12),
        "INTERNAL" => return Some(13),
        "UNAVAILABLE" => return Some(14),
        "DATA_LOSS" | "DATALOSS" => return Some(15),
        _ => {}
    }
    None
}

impl<'a> BorrowedAggregationKey<'a> {
    /// Return an AggregationKey matching the given span.
    ///
    /// If `peer_tags_keys` is not empty then the peer tags of the span will be included in the
    /// key.
    pub(super) fn from_span<T: StatSpan<'a>>(
        resource_name: String,
        span: &'a T,
        peer_tag_keys: &'a [String],
    ) -> Self {
        let span_kind = span.get_meta(TAG_SPANKIND).unwrap_or_default();
        let peer_tags = if should_track_peer_tags(span_kind) {
            // Parse the meta tags of the span and return a list of the peer tags based on the list
            // of `peer_tag_keys`
            peer_tag_keys
                .iter()
                .filter_map(|key| Some(((key.as_str()), (span.get_meta(key.as_str())?))))
                .collect()
        } else if let Some(base_service) = span.get_meta("_dd.base_service") {
            // Internal spans with a base service override use only _dd.base_service as peer tag
            vec![("_dd.base_service", base_service)]
        } else {
            vec![]
        };

        let http_method = span.get_meta("http.method").unwrap_or_default();

        let http_endpoint = span
            .get_meta("http.endpoint")
            .or_else(|| span.get_meta("http.route"))
            .unwrap_or_default();

        let status_code = if let Some(status_code) = span.get_metrics(TAG_STATUS_CODE) {
            status_code as u32
        } else if let Some(status_code) = span.get_meta(TAG_STATUS_CODE) {
            status_code.parse().unwrap_or_default()
        } else {
            0
        };

        let grpc_status_code = get_grpc_status_code(span);

        let service_source = span.get_meta(TAG_SVC_SRC).unwrap_or_default();

        Self {
            resource_name,
            service_name: span.service(),
            operation_name: span.name(),
            span_type: span.r#type(),
            span_kind,
            http_status_code: status_code,
            is_synthetics_request: span
                .get_meta(TAG_ORIGIN)
                .is_some_and(|origin| origin.starts_with(TAG_SYNTHETICS)),
            peer_tags,
            is_trace_root: span.is_trace_root(),
            http_method,
            http_endpoint,
            grpc_status_code,
            service_source,
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
            grpc_status_code: value.grpc_status_code.parse().ok(),
            service_source: value.service_source,
        }
    }
}

/// Return true if we care about peer tags on the span
fn should_track_peer_tags<T>(span_kind: T) -> bool
where
    T: SpanText,
{
    matches!(
        span_kind.borrow().to_lowercase().as_str(),
        "client" | "producer" | "consumer"
    )
}

/// The stats computed from a group of span with the same AggregationKey
#[derive(Debug, Default, Clone)]
pub(super) struct GroupedStats {
    hits: u64,
    errors: u64,
    duration: u64,
    top_level_hits: u64,
    ok_summary: libdd_ddsketch::DDSketch,
    error_summary: libdd_ddsketch::DDSketch,
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
        grpc_status_code: key
            .grpc_status_code
            .map(|c| c.to_string())
            .unwrap_or_default(),
        service_source: key.service_source,
        span_derived_primary_tags: vec![], // Todo
    }
}

#[cfg(test)]
mod tests {
    use libdd_trace_utils::span::v04::{SpanBytes, SpanSlice};

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
            // Span with grpc status from meta as named string
            (
                SpanBytes {
                    meta: HashMap::from([("rpc.grpc.status_code".into(), "OK".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    grpc_status_code: Some(0),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with grpc status from meta as numeric string
            (
                SpanBytes {
                    meta: HashMap::from([("rpc.grpc.status_code".into(), "14".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    grpc_status_code: Some(14),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with grpc status from meta with StatusCode. prefix
            (
                SpanBytes {
                    meta: HashMap::from([("grpc.code".into(), "StatusCode.UNAVAILABLE".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    grpc_status_code: Some(14),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with grpc status from metrics takes precedence over meta
            (
                SpanBytes {
                    meta: HashMap::from([(
                        "rpc.grpc.status_code".into(),
                        "PERMISSION_DENIED".into(),
                    )]),
                    metrics: HashMap::from([("rpc.grpc.status_code".into(), 2.0)]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    grpc_status_code: Some(7),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with grpc status from metrics via secondary key
            (
                SpanBytes {
                    metrics: HashMap::from([("grpc.code".into(), 3.0)]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    grpc_status_code: Some(3),
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with invalid grpc status string
            (
                SpanBytes {
                    meta: HashMap::from([("rpc.grpc.status_code".into(), "NOPE".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    is_trace_root: true,
                    ..Default::default()
                },
            ),
            // Span with service source set by integration
            (
                SpanBytes {
                    service: "my-service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("_dd.svc_src".into(), "redis".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: true,
                    service_source: "redis".into(),
                    ..Default::default()
                },
            ),
            // Span with service source set by configuration option
            (
                SpanBytes {
                    service: "my-service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: HashMap::from([("_dd.svc_src".into(), "opt.split_by_tag".into())]),
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: true,
                    service_source: "opt.split_by_tag".into(),
                    ..Default::default()
                },
            ),
            // Span without service source (default service name)
            (
                SpanBytes {
                    service: "my-service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    ..Default::default()
                },
                OwnedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: true,
                    service_source: "".into(),
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
            let borrowed_key =
                BorrowedAggregationKey::from_span(span.resource().to_owned(), &span, &[]);
            assert_eq!(
                OwnedAggregationKey::from(&borrowed_key),
                expected_key,
                "for span {span:?}"
            );
            assert_eq!(
                get_hash(&borrowed_key),
                get_hash(&OwnedAggregationKey::from(&borrowed_key))
            );
        }

        for (span, expected_key) in test_cases_with_peer_tags {
            let borrowed_key = BorrowedAggregationKey::from_span(
                span.resource().to_owned(),
                &span,
                test_peer_tags.as_slice(),
            );
            assert_eq!(OwnedAggregationKey::from(&borrowed_key), expected_key);
            assert_eq!(
                get_hash(&borrowed_key),
                get_hash(&OwnedAggregationKey::from(&borrowed_key))
            );
        }
    }

    #[test]
    fn test_grpc_status_str_to_int_value() {
        // Numeric strings parse directly
        assert_eq!(grpc_status_str_to_int_value("0"), Some(0));
        assert_eq!(grpc_status_str_to_int_value("14"), Some(14));
        assert_eq!(grpc_status_str_to_int_value("255"), Some(255));
        assert_eq!(grpc_status_str_to_int_value("256"), None);
        assert_eq!(grpc_status_str_to_int_value("-1"), None);

        // Named status codes (uppercase)
        assert_eq!(grpc_status_str_to_int_value("OK"), Some(0));
        assert_eq!(grpc_status_str_to_int_value("CANCELLED"), Some(1));
        assert_eq!(grpc_status_str_to_int_value("UNKNOWN"), Some(2));
        assert_eq!(grpc_status_str_to_int_value("INVALID_ARGUMENT"), Some(3));
        assert_eq!(grpc_status_str_to_int_value("DEADLINE_EXCEEDED"), Some(4));
        assert_eq!(grpc_status_str_to_int_value("NOT_FOUND"), Some(5));
        assert_eq!(grpc_status_str_to_int_value("ALREADY_EXISTS"), Some(6));
        assert_eq!(grpc_status_str_to_int_value("PERMISSION_DENIED"), Some(7));
        assert_eq!(grpc_status_str_to_int_value("UNAUTHENTICATED"), Some(16));
        assert_eq!(grpc_status_str_to_int_value("RESOURCE_EXHAUSTED"), Some(8));
        assert_eq!(grpc_status_str_to_int_value("FAILED_PRECONDITION"), Some(9));
        assert_eq!(grpc_status_str_to_int_value("ABORTED"), Some(10));
        assert_eq!(grpc_status_str_to_int_value("OUT_OF_RANGE"), Some(11));
        assert_eq!(grpc_status_str_to_int_value("UNIMPLEMENTED"), Some(12));
        assert_eq!(grpc_status_str_to_int_value("INTERNAL"), Some(13));
        assert_eq!(grpc_status_str_to_int_value("UNAVAILABLE"), Some(14));
        assert_eq!(grpc_status_str_to_int_value("DATA_LOSS"), Some(15));

        // Case-insensitive matching
        assert_eq!(grpc_status_str_to_int_value("ok"), Some(0));
        assert_eq!(grpc_status_str_to_int_value("Cancelled"), Some(1));
        assert_eq!(grpc_status_str_to_int_value("not_found"), Some(5));

        // StatusCode. prefix is stripped
        assert_eq!(grpc_status_str_to_int_value("StatusCode.OK"), Some(0));
        assert_eq!(
            grpc_status_str_to_int_value("StatusCode.UNAVAILABLE"),
            Some(14)
        );
        assert_eq!(
            grpc_status_str_to_int_value("StatusCode.not_found"),
            Some(5)
        );

        // Alternate spellings
        assert_eq!(grpc_status_str_to_int_value("CANCELED"), Some(1));

        // Unknown / empty strings
        assert_eq!(grpc_status_str_to_int_value("NOPE"), None);
        assert_eq!(grpc_status_str_to_int_value(""), None);
        assert_eq!(
            grpc_status_str_to_int_value("this_is_a_kinda_long_string_that_needs_upcasing"),
            None
        );

        // Non ascii
        assert_eq!(grpc_status_str_to_int_value("🤣"), None);
    }
}
