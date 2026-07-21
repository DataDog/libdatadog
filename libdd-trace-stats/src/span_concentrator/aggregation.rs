// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.

use hashbrown::{HashMap, HashSet};
use libdd_common::tag::const_assert;
use libdd_trace_obfuscation::ip_address::quantize_peer_ip_addresses;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::SpanText;
use std::{
    borrow::{Borrow, Cow},
    hash::{DefaultHasher, Hash, Hasher as _},
};
use tracing::warn;

use crate::span_concentrator::StatSpan;

use super::CardinalityLimitConfig;

/// Sentinel value used for cardinality limiting.
pub const TRACER_BLOCKED_VALUE: &str = "tracer_blocked_value";

const TAG_STATUS_CODE: &str = "http.status_code";
const ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN: usize = 200;
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

/// Aggregation key fields shared across all concentrator implementations — everything
/// **except** peer tags.
///
/// `T` is the string representation:
/// * `&'a str`   — borrowed references used in [`BorrowedAggregationKey`]
/// * `String`    — owned values used in `OwnedAggregationKey`
/// * `StringRef` — offset+len into a SHM string pool, used in `ShmKeyHeader`
#[derive(
    Clone, Default, Hash, Eq, PartialEq, Debug, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct FixedAggregationKey<T> {
    pub resource_name: T,
    pub service_name: T,
    pub operation_name: T,
    pub span_type: T,
    pub span_kind: T,
    pub http_method: T,
    pub http_endpoint: T,
    pub service_source: T,
    pub http_status_code: u32,
    pub grpc_status_code: Option<u8>,
    pub is_synthetics_request: bool,
    pub is_trace_root: pb::Trilean,
}

impl<T> FixedAggregationKey<T> {
    /// Map all string fields through `f`, preserving scalar fields.
    pub fn convert<'a, V: 'a, I: ?Sized + 'a, F: Fn(&'a I) -> V>(
        &'a self,
        f: F,
    ) -> FixedAggregationKey<V>
    where
        T: Borrow<I>,
    {
        FixedAggregationKey {
            resource_name: f(self.resource_name.borrow()),
            service_name: f(self.service_name.borrow()),
            operation_name: f(self.operation_name.borrow()),
            span_type: f(self.span_type.borrow()),
            span_kind: f(self.span_kind.borrow()),
            http_method: f(self.http_method.borrow()),
            http_endpoint: f(self.http_endpoint.borrow()),
            service_source: f(self.service_source.borrow()),
            http_status_code: self.http_status_code,
            grpc_status_code: self.grpc_status_code,
            is_synthetics_request: self.is_synthetics_request,
            is_trace_root: self.is_trace_root,
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
/// Represent a stats aggregation key borrowed from span data
pub(super) struct BorrowedAggregationKey<'a> {
    fixed: FixedAggregationKey<&'a str>,
    peer_tags: Vec<(&'a str, Cow<'a, str>)>,
    additional_metric_tags: Vec<(&'a str, &'a str)>,
}

impl hashbrown::Equivalent<OwnedAggregationKey> for BorrowedAggregationKey<'_> {
    #[inline]
    fn equivalent(&self, other: &OwnedAggregationKey) -> bool {
        self.fixed == other.fixed.convert(|s| s)
            && self.peer_tags.len() == other.peer_tags.len()
            && self
                .peer_tags
                .iter()
                .zip(other.peer_tags.iter())
                .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
            && self.additional_metric_tags.len() == other.additional_metric_tags.len()
            && self
                .additional_metric_tags
                .iter()
                .zip(other.additional_metric_tags.iter())
                .all(|((k1, v1), (k2, v2))| k1 == k2 && v1 == v2)
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
    fixed: FixedAggregationKey<String>,
    peer_tags: Vec<(String, String)>,
    additional_metric_tags: Vec<(String, String)>,
}

impl From<&BorrowedAggregationKey<'_>> for OwnedAggregationKey {
    fn from(value: &BorrowedAggregationKey<'_>) -> Self {
        OwnedAggregationKey {
            fixed: value.fixed.convert(str::to_owned),
            peer_tags: value
                .peer_tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            additional_metric_tags: value
                .additional_metric_tags
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
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
    /// If `peer_tag_keys` is not empty then the peer tags of the span will be included in the
    /// key.
    /// If `additional_metric_tags` is not empty then matching span tags keys are included in the
    /// key.
    pub(super) fn from_span<T: StatSpan<'a>>(
        span: &'a T,
        peer_tag_keys: &'a [String],
        additional_metric_tag_keys: &'a [String],
    ) -> Self {
        Self::from_obfuscated_span(
            span.resource(),
            span,
            peer_tag_keys,
            additional_metric_tag_keys,
        )
    }

    pub(crate) fn from_obfuscated_span<'b, T>(
        resource_name: &'a str,
        span: &'b T,
        peer_tag_keys: &'b [String],
        additional_metric_tag_keys: &'b [String],
    ) -> BorrowedAggregationKey<'a>
    where
        T: StatSpan<'b>,
        // resource_name is a temporary string on the stack the span will outlive it
        'b: 'a,
    {
        let span_kind = span.get_meta(TAG_SPANKIND).unwrap_or_default();
        let peer_tags = if should_track_peer_tags(span_kind) {
            // Parse the meta tags of the span and return a list of the peer tags based on the list
            // of `peer_tag_keys`. IP address values are quantized to reduce cardinality.
            peer_tag_keys
                .iter()
                .filter_map(|key| {
                    let value = span.get_meta(key.as_str())?;
                    Some((key.as_str(), quantize_peer_ip_addresses(value)))
                })
                .collect()
        } else if let Some(base_service) = span.get_meta("_dd.base_service") {
            // Internal spans with a base service override use only _dd.base_service as peer tag
            vec![("_dd.base_service", Cow::Borrowed(base_service))]
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

        let additional_metric_tags: Vec<(&'a str, &'a str)> = additional_metric_tag_keys
            .iter()
            .filter_map(|key| match span.get_meta(key.as_str()) {
                Some(v) if !v.is_empty() => {
                    // Byte length >= char count, so skip the char walk when byte length alone
                    // is within the max character length, otherwise stop as soon as we pass the max character length.
                    if v.len() > ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN
                        && v.chars().nth(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN).is_some()
                    {
                        warn!(
                            "additional_metric_tags: value for key '{}' exceeds {} characters; substituting tracer_blocked_value",
                            key, ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN,
                        );
                        Some((key.as_str(), TRACER_BLOCKED_VALUE))
                    } else {
                        Some((key.as_str(), v))
                    }
                }
                _ => None,
            })
            .collect();

        Self {
            fixed: FixedAggregationKey {
                resource_name,
                service_name: span.service(),
                operation_name: span.name(),
                span_type: span.r#type(),
                span_kind,
                http_method,
                http_endpoint,
                service_source,
                http_status_code: status_code,
                grpc_status_code,
                is_synthetics_request: span
                    .get_meta(TAG_ORIGIN)
                    .is_some_and(|origin| origin.starts_with(TAG_SYNTHETICS)),
                is_trace_root: if span.is_trace_root() {
                    pb::Trilean::True
                } else {
                    pb::Trilean::False
                },
            },
            peer_tags,
            additional_metric_tags,
        }
    }
}

impl OwnedAggregationKey {
    /// Return the overflow sentinel key.
    pub(super) fn overflow_key() -> Self {
        OwnedAggregationKey {
            fixed: FixedAggregationKey {
                resource_name: TRACER_BLOCKED_VALUE.to_owned(),
                service_name: TRACER_BLOCKED_VALUE.to_owned(),
                operation_name: TRACER_BLOCKED_VALUE.to_owned(),
                span_type: TRACER_BLOCKED_VALUE.to_owned(),
                span_kind: TRACER_BLOCKED_VALUE.to_owned(),
                http_method: TRACER_BLOCKED_VALUE.to_owned(),
                http_endpoint: TRACER_BLOCKED_VALUE.to_owned(),
                service_source: TRACER_BLOCKED_VALUE.to_owned(),
                http_status_code: 0,
                grpc_status_code: None,
                is_synthetics_request: false,
                is_trace_root: pb::Trilean::NotSet,
            },
            peer_tags: vec![(TRACER_BLOCKED_VALUE.to_owned(), "".to_owned())],
            additional_metric_tags: vec![(TRACER_BLOCKED_VALUE.to_owned(), "".to_owned())],
        }
    }
}

impl From<pb::ClientGroupedStats> for OwnedAggregationKey {
    fn from(value: pb::ClientGroupedStats) -> Self {
        Self {
            fixed: FixedAggregationKey {
                resource_name: value.resource,
                service_name: value.service,
                operation_name: value.name,
                span_type: value.r#type,
                span_kind: value.span_kind,
                http_method: value.http_method,
                http_endpoint: value.http_endpoint,
                service_source: value.service_source,
                http_status_code: value.http_status_code,
                grpc_status_code: value.grpc_status_code.parse().ok(),
                is_synthetics_request: value.synthetics,
                is_trace_root: pb::Trilean::try_from(value.is_trace_root)
                    .unwrap_or(pb::Trilean::NotSet),
            },
            peer_tags: value
                .peer_tags
                .into_iter()
                .filter_map(|t| {
                    let (key, value) = t.split_once(':')?;
                    Some((key.to_string(), value.to_string()))
                })
                .collect(),
            additional_metric_tags: value
                .additional_metric_tags
                .into_iter()
                .filter_map(|t| {
                    let (key, value) = t.split_once(':')?;
                    Some((key.to_string(), value.to_string()))
                })
                .collect(),
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
    // Exact per-cell (ok/error) scalars used by the OTLP trace-metrics path. These are tracked
    // separately from `duration` so the /v0.6/stats agent payload is byte-for-byte unchanged.
    ok_duration: u64,
    ok_min: u64,
    ok_max: u64,
    error_duration: u64,
    error_min: u64,
    error_max: u64,
}

impl GroupedStats {
    /// Update the stats of a GroupedStats by inserting a span.
    fn insert(&mut self, duration: i64, is_error: bool, is_top_level: bool) {
        self.hits += 1;
        self.duration += duration as u64;
        let d = duration as u64;
        if is_error {
            self.errors += 1;
            let _ = self.error_summary.add(duration as f64);
            self.error_duration += d;
            self.error_min = if self.errors == 1 {
                d
            } else {
                self.error_min.min(d)
            };
            self.error_max = self.error_max.max(d);
        } else {
            let _ = self.ok_summary.add(duration as f64);
            self.ok_duration += d;
            let ok_count = self.hits - self.errors;
            self.ok_min = if ok_count == 1 { d } else { self.ok_min.min(d) };
            self.ok_max = self.ok_max.max(d);
        }
        if is_top_level {
            self.top_level_hits += 1;
        }
    }
}

/// Exact per-cell (ok or error) scalars for one aggregation group, surfaced to the OTLP
/// trace-metrics path. `count` is exact; `duration_ns`/`min_ns`/`max_ns` are exact when
/// `count > 0` and meaningless otherwise (the OTLP mapper suppresses empty cells).
#[derive(Debug, Clone, Copy, Default)]
pub struct OtlpExactCell {
    pub count: u64,
    pub duration_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
}

/// Exact OK/ERROR cells for one aggregation group, in the same order as the `stats` vector
/// of the accompanying [`pb::ClientStatsBucket`].
#[derive(Debug, Clone, Default)]
pub struct OtlpExactGroup {
    pub ok: OtlpExactCell,
    pub error: OtlpExactCell,
}

/// A bucket flushed for the OTLP trace-metrics path. `exact[i]` is the exact-scalar sidecar
/// for `bucket.stats[i]`; the protobuf bucket itself is identical to what the agent path uses.
#[derive(Debug, Clone)]
pub struct OtlpStatsBucket {
    pub bucket: pb::ClientStatsBucket,
    pub exact: Vec<OtlpExactGroup>,
}

/// A time bucket used for stats aggregation. It stores a map of GroupedStats storing the stats of
/// spans aggregated on their AggregationKey.
#[derive(Debug, Clone)]
pub(super) struct StatsBucket {
    data: HashMap<OwnedAggregationKey, GroupedStats>,
    start: u64,
    /// Maximum number of distinct aggregation keys this bucket will hold before collapsing new
    /// ones into the overflow sentinel key.
    cardinality_limits: CardinalityLimitConfig,
    // HashSet of hashes of field values so we save memory
    // This is not 100% accurate but the probability of getting collision is close to 0
    // In the very rare case we get a collision, we would get one extra bucket which is totally
    // fine
    distinct_resources: HashSet<u64>,
    distinct_http_endpoints: HashSet<u64>,
    distinct_peer_tags: HashSet<u64>,
    distinct_additional_tags: HashSet<u64>,
    /// Number of spans collapsed into the overflow bucket due to whole-key cardinality limiting.
    collapsed_count: u64,
    collapsed_fields_metrics: CollapsedFieldsMetrics,
    /// Indicates if stats obfuscated in this bucket. This is set once at creation and stays
    /// constant per bucket
    #[cfg(feature = "stats-obfuscation")]
    pub(super) obfuscated: bool,
}

#[repr(transparent)]
pub struct CollapsedField;
impl CollapsedField {
    pub const RESOURCE_NAME: usize = 1 << 1;
    pub const HTTP_ENDPOINT: usize = 1 << 2;
    pub const PEER_TAGS: usize = 1 << 3;
    #[allow(
        unused,
        reason = "FIXME(SVLS-8787|github.com/DataDog/libdatadog/pull/2170): implement stats additional tags"
    )]
    pub const ADDITIONAL_TAGS: usize = 1 << 4;
    pub const COUNT: u8 = 5;
}

const COLLAPSED_FIELD_METRIC_SIZE: usize = 1 << CollapsedField::COUNT;
#[derive(Debug, Clone, Default, Copy)]
// Note: slot 0 is a counter for non_collapsed spans. It's not used for emitting telemetry
pub struct CollapsedFieldsMetrics([usize; COLLAPSED_FIELD_METRIC_SIZE]);

const_assert!(COLLAPSED_FIELD_METRIC_SIZE <= 32); // Metrics table is of reasonable size

impl CollapsedFieldsMetrics {
    pub fn zero() -> Self {
        Self::default()
    }

    #[cfg(feature = "dogstatsd")]
    pub fn emit_dogstatsd(&self, dogstatsd: &libdd_dogstatsd_client::DogStatsDClient) {
        // skip the first slot that is used to count span which have no collapsed fields.
        for (mask, &count) in self.0.iter().enumerate().skip(1) {
            if count > 0 {
                let mut tags = Vec::new();
                for field_pow in 1..CollapsedField::COUNT {
                    let field_value = 1 << field_pow;
                    assert!([
                        CollapsedField::RESOURCE_NAME,
                        CollapsedField::HTTP_ENDPOINT,
                        CollapsedField::PEER_TAGS,
                        CollapsedField::ADDITIONAL_TAGS
                    ]
                    .contains(&field_value));
                    let has_field = (mask & field_value) != 0;
                    if !has_field {
                        continue;
                    }
                    let field_tag = match field_value {
                        CollapsedField::RESOURCE_NAME => {
                            libdd_common::tag!("collapsed_spans", "resource")
                        }
                        CollapsedField::HTTP_ENDPOINT => {
                            libdd_common::tag!("collapsed_spans", "http_endpoint")
                        }
                        CollapsedField::PEER_TAGS => {
                            libdd_common::tag!("collapsed_spans", "peer_tags")
                        }
                        CollapsedField::ADDITIONAL_TAGS => {
                            libdd_common::tag!("collapsed_spans", "additional_metric_tags")
                        }
                        #[allow(
                            clippy::unreachable,
                            reason = "field pow is between 1..CollapsedField::COUNT, so field_value is a valid CollapsedField value. (Asserted just above)"
                        )]
                        _ => unreachable!(),
                    };
                    tags.push(field_tag);
                }
                assert!(!tags.is_empty());
                dogstatsd.send(vec![libdd_dogstatsd_client::DogStatsDAction::Count(
                    "datadog.tracer.stats.collapsed_spans",
                    count as i64,
                    tags.iter(),
                )]);
            }
        }
    }

    #[cfg(feature = "telemetry")]
    pub fn emit_telemetry<
        Cap: libdd_capabilities::HttpClientCapability
            + libdd_capabilities::SleepCapability
            + libdd_capabilities::MaybeSend
            + Sync
            + 'static,
    >(
        &self,
        handle: &libdd_telemetry::worker::TelemetryWorkerHandle<Cap>,
        context_key: &libdd_telemetry::metrics::ContextKey,
    ) {
        // skip the first slot that is used to count span which have no collapsed fields.
        for (mask, &count) in self.0.iter().enumerate().skip(1) {
            if count > 0 {
                let mut tags = Vec::new();
                for field_pow in 1..CollapsedField::COUNT {
                    let field_value = 1 << field_pow;
                    assert!([
                        CollapsedField::RESOURCE_NAME,
                        CollapsedField::HTTP_ENDPOINT,
                        CollapsedField::PEER_TAGS,
                        CollapsedField::ADDITIONAL_TAGS
                    ]
                    .contains(&field_value));
                    let has_field = (mask & field_value) != 0;
                    if !has_field {
                        continue;
                    }
                    let field_tag = match field_value {
                        CollapsedField::RESOURCE_NAME => {
                            libdd_common::tag!("collapsed_spans", "resource")
                        }
                        CollapsedField::HTTP_ENDPOINT => {
                            libdd_common::tag!("collapsed_spans", "http_endpoint")
                        }
                        CollapsedField::PEER_TAGS => {
                            libdd_common::tag!("collapsed_spans", "peer_tags")
                        }
                        CollapsedField::ADDITIONAL_TAGS => {
                            libdd_common::tag!("collapsed_spans", "additional_metric_tags")
                        }
                        #[allow(
                            clippy::unreachable,
                            reason = "field pow is between 1..CollapsedField::COUNT, so field_value is a valid CollapsedField value. (Asserted just above)"
                        )]
                        _ => unreachable!(),
                    };
                    tags.push(field_tag);
                }
                assert!(!tags.is_empty());
                let _ = handle.add_point(count as f64, context_key, tags);
            }
        }
    }
}

impl std::ops::AddAssign for CollapsedFieldsMetrics {
    fn add_assign(&mut self, rhs: Self) {
        for i in 0..self.0.len() {
            self.0[i] += rhs.0[i];
        }
    }
}

impl StatsBucket {
    /// Return a new StatsBucket starting at `start_timestamp`.
    ///
    /// `cardinality_limits` are the values for whole-key and per-field cardinality limits
    pub(super) fn new(
        start_timestamp: u64,
        cardinality_limits: CardinalityLimitConfig,
        #[cfg(feature = "stats-obfuscation")] obfuscation_enabled: bool,
    ) -> Self {
        Self {
            data: HashMap::new(),
            start: start_timestamp,
            cardinality_limits,
            collapsed_count: 0,
            #[cfg(feature = "stats-obfuscation")]
            obfuscated: obfuscation_enabled,
            distinct_resources: HashSet::new(),
            distinct_http_endpoints: HashSet::new(),
            distinct_peer_tags: HashSet::new(),
            distinct_additional_tags: HashSet::new(),
            collapsed_fields_metrics: CollapsedFieldsMetrics::zero(),
        }
    }

    /// Returns metrics on spans field collapse with reasons.
    pub fn collapsed_fields_metrics(&self) -> CollapsedFieldsMetrics {
        self.collapsed_fields_metrics
    }

    /// Return the number of spans collapsed into the overflow bucket.
    pub(super) fn collapsed_count(&self) -> u64 {
        self.collapsed_count
    }

    /// Insert a value as stats in the group corresponding to the aggregation key, if it does not
    /// exist it creates it.
    ///
    /// Keys that already exist in this bucket always merge normally. A new key is subject to the
    /// `max_entries` limit, which collapses it into the overflow sentinel key.
    pub(super) fn insert(
        &mut self,
        mut key: BorrowedAggregationKey<'_>,
        duration: i64,
        is_error: bool,
        is_top_level: bool,
    ) {
        // Per field cardinality limiting
        self.collapse_key_fields_cardinality(&mut key);

        // The map can't change size before the entry below is resolved, so this single read
        // covers the `max_entries` check in the vacant branch without a further lookup.
        let len_before_insert = self.data.len();

        match self.data.entry_ref(&key) {
            // Existing key, merge
            hashbrown::hash_map::EntryRef::Occupied(mut e) => {
                e.get_mut().insert(duration, is_error, is_top_level);
            }
            hashbrown::hash_map::EntryRef::Vacant(e) => {
                // New key over the max entry limit, collapse into the overflow
                // sentinel.
                if len_before_insert >= self.cardinality_limits.whole_key_limit {
                    self.collapsed_count += 1;
                    self.data
                        .entry(OwnedAggregationKey::overflow_key())
                        .or_default()
                        .insert(duration, is_error, is_top_level);
                    return;
                }
                // Within the max entry limit, admit key as a new distinct entry.
                e.insert(GroupedStats::default())
                    .insert(duration, is_error, is_top_level);
            }
        }
    }

    /// Collapse an aggregation key fields following the bucket's `CardinalityLimitConfig`
    fn collapse_key_fields_cardinality(&mut self, key: &mut BorrowedAggregationKey<'_>) {
        use hashbrown::hash_set::Entry;
        fn hash(input: &impl Hash) -> u64 {
            let mut hasher = DefaultHasher::new();
            input.hash(&mut hasher);
            hasher.finish()
        }

        let mut collapsed_fields = 0;

        let resource_hash = hash(&key.fixed.resource_name);
        let resources_count = self.distinct_resources.len();
        if let Entry::Vacant(slot) = self.distinct_resources.entry(resource_hash) {
            if resources_count >= self.cardinality_limits.resource_limit {
                key.fixed.resource_name = TRACER_BLOCKED_VALUE;
                collapsed_fields |= CollapsedField::RESOURCE_NAME;
            } else {
                slot.insert();
            }
        }

        let http_endpoint_hash = hash(&key.fixed.http_endpoint);
        let http_endpoints_count = self.distinct_http_endpoints.len();
        if let Entry::Vacant(slot) = self.distinct_http_endpoints.entry(http_endpoint_hash) {
            if http_endpoints_count >= self.cardinality_limits.http_endpoint_limit {
                key.fixed.http_endpoint = TRACER_BLOCKED_VALUE;
                collapsed_fields |= CollapsedField::HTTP_ENDPOINT;
            } else {
                slot.insert();
            }
        }

        let peer_tags_hash = hash(&key.peer_tags);
        let peer_tags_count = self.distinct_peer_tags.len();
        if let Entry::Vacant(slot) = self.distinct_peer_tags.entry(peer_tags_hash) {
            if peer_tags_count >= self.cardinality_limits.peer_tags_limit {
                key.peer_tags = vec![(TRACER_BLOCKED_VALUE, Cow::Borrowed(""))];
                collapsed_fields |= CollapsedField::PEER_TAGS;
            } else {
                slot.insert();
            }
        }

        let additional_tags_hash = hash(&key.additional_metric_tags);
        let additional_tags_count = self.distinct_additional_tags.len();
        if let Entry::Vacant(slot) = self.distinct_additional_tags.entry(additional_tags_hash) {
            if additional_tags_count >= self.cardinality_limits.additional_tags_limit {
                key.additional_metric_tags = vec![(TRACER_BLOCKED_VALUE, "")];
            } else {
                slot.insert();
            }
        }
        self.collapsed_fields_metrics.0[collapsed_fields] += 1;
    }

    /// Consume the bucket and return a ClientStatsBucket containing the bucket stats.
    /// `bucket_duration` is the size of buckets for the concentrator containing the bucket.
    pub(super) fn flush(self, bucket_duration: u64) -> pb::ClientStatsBucket {
        self.flush_with_otlp_exact(bucket_duration).bucket
    }

    /// Like [`Self::flush`], but additionally produces exact per-cell scalars for the OTLP
    /// trace-metrics path. The `bucket` field is identical to what [`Self::flush`] returns.
    pub(super) fn flush_with_otlp_exact(self, bucket_duration: u64) -> OtlpStatsBucket {
        let mut stats = Vec::with_capacity(self.data.len());
        let mut exact = Vec::with_capacity(self.data.len());
        for (k, g) in self.data {
            exact.push(OtlpExactGroup {
                ok: OtlpExactCell {
                    count: g.hits.saturating_sub(g.errors),
                    duration_ns: g.ok_duration,
                    min_ns: g.ok_min,
                    max_ns: g.ok_max,
                },
                error: OtlpExactCell {
                    count: g.errors,
                    duration_ns: g.error_duration,
                    min_ns: g.error_min,
                    max_ns: g.error_max,
                },
            });
            stats.push(encode_grouped_stats(k, g));
        }
        OtlpStatsBucket {
            bucket: pb::ClientStatsBucket {
                start: self.start,
                duration: bucket_duration,
                stats,
                agent_time_shift: 0,
            },
            exact,
        }
    }
}

/// Create a ClientGroupedStats struct based on the given AggregationKey and GroupedStats
fn encode_grouped_stats(key: OwnedAggregationKey, group: GroupedStats) -> pb::ClientGroupedStats {
    let f = key.fixed;
    pb::ClientGroupedStats {
        service: f.service_name,
        name: f.operation_name,
        resource: f.resource_name,
        http_status_code: f.http_status_code,
        r#type: f.span_type,
        db_type: String::new(), // db_type is not used yet (see proto definition)

        hits: group.hits,
        errors: group.errors,
        duration: group.duration,

        ok_summary: group.ok_summary.encode_to_vec(),
        error_summary: group.error_summary.encode_to_vec(),
        synthetics: f.is_synthetics_request,
        top_level_hits: group.top_level_hits,
        span_kind: f.span_kind,

        peer_tags: key
            .peer_tags
            .into_iter()
            .map(|(k, v)| {
                if v.is_empty() {
                    k.to_string()
                } else {
                    format!("{k}:{v}")
                }
            })
            .collect(),
        is_trace_root: f.is_trace_root.into(),
        http_method: f.http_method,
        http_endpoint: f.http_endpoint,
        grpc_status_code: f
            .grpc_status_code
            .map(|c| c.to_string())
            .unwrap_or_default(),
        service_source: f.service_source,
        span_derived_primary_tags: vec![],
        additional_metric_tags: key
            .additional_metric_tags
            .into_iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use libdd_trace_utils::span::v04::{SpanBytes, SpanSlice};

    use super::*;
    use libdd_trace_protobuf::pb;
    use std::hash::Hash;

    fn get_hash(v: &impl Hash) -> u64 {
        use std::hash::Hasher;
        let mut hasher = std::hash::DefaultHasher::new();
        v.hash(&mut hasher);
        hasher.finish()
    }

    impl FixedAggregationKey<String> {
        fn into_key(self) -> OwnedAggregationKey {
            OwnedAggregationKey {
                fixed: self,
                peer_tags: vec![],
                additional_metric_tags: vec![],
            }
        }
        fn into_key_with_peers(self, peer_tags: Vec<(String, String)>) -> OwnedAggregationKey {
            OwnedAggregationKey {
                fixed: self,
                peer_tags,
                additional_metric_tags: vec![],
            }
        }
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
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with span kind
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("span.kind".into(), "client".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with peer tags but peertags aggregation disabled
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("span.kind".into(), "client".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with multiple peer tags but peertags aggregation disabled
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("span.kind".into(), "producer".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "producer".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
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
                    meta: vec![
                        ("span.kind".into(), "server".into()),
                        ("aws.s3.bucket".into(), "bucket-a".into()),
                        ("db.instance".into(), "dynamo.test.us1".into()),
                        ("db.system".into(), "dynamodb".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "server".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span from synthetics
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("_dd.origin".into(), "synthetics-browser".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: true,
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with status code in meta
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.status_code".into(), "418".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 418,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with invalid status code in meta
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.status_code".into(), "x".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with status code in metrics
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: vec![("http.status_code".into(), 418.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 418,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with http.method and http.route
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "GET /api/v1/users".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.method".into(), "GET".into()),
                        ("http.route".into(), "/api/v1/users".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "GET /api/v1/users".into(),
                    http_method: "GET".into(),
                    http_endpoint: "/api/v1/users".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with http.method and http.endpoint (http.endpoint takes precedence)
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "POST /users/create".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.method".into(), "POST".into()),
                        ("http.route".into(), "/users/create".into()),
                        ("http.endpoint".into(), "/users/create2".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "POST /users/create".into(),
                    http_method: "POST".into(),
                    http_endpoint: "/users/create2".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with grpc status from meta as named string
            (
                SpanBytes {
                    meta: vec![("rpc.grpc.status_code".into(), "OK".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    grpc_status_code: Some(0),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // grpc.method.name is carried in GroupedStats (for OTLP), not in the aggregation key.
            (
                SpanBytes {
                    meta: vec![("grpc.method.name".into(), "/pkg.Svc/Method".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with grpc status from meta as numeric string
            (
                SpanBytes {
                    meta: vec![("rpc.grpc.status_code".into(), "14".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    grpc_status_code: Some(14),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with grpc status from meta with StatusCode. prefix
            (
                SpanBytes {
                    meta: vec![("grpc.code".into(), "StatusCode.UNAVAILABLE".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    grpc_status_code: Some(14),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with grpc status from metrics takes precedence over meta
            (
                SpanBytes {
                    meta: vec![("rpc.grpc.status_code".into(), "PERMISSION_DENIED".into())].into(),
                    metrics: vec![("rpc.grpc.status_code".into(), 2.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    grpc_status_code: Some(7),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with grpc status from metrics via secondary key
            (
                SpanBytes {
                    metrics: vec![("grpc.code".into(), 3.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    grpc_status_code: Some(3),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with invalid grpc status string
            (
                SpanBytes {
                    meta: vec![("rpc.grpc.status_code".into(), "NOPE".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with service source set by integration
            (
                SpanBytes {
                    service: "my-service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("_dd.svc_src".into(), "redis".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: pb::Trilean::True,
                    service_source: "redis".into(),
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span with service source set by configuration option
            (
                SpanBytes {
                    service: "my-service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("_dd.svc_src".into(), "opt.split_by_tag".into())].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: pb::Trilean::True,
                    service_source: "opt.split_by_tag".into(),
                    ..Default::default()
                }
                .into_key(),
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
                FixedAggregationKey {
                    service_name: "my-service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_trace_root: pb::Trilean::True,
                    service_source: "".into(),
                    ..Default::default()
                }
                .into_key(),
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
                    meta: vec![("span.kind", "client"), ("aws.s3.bucket", "bucket-a")].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "client".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key_with_peers(vec![("aws.s3.bucket".into(), "bucket-a".into())]),
            ),
            // Span with multiple peer tags with peertags aggregation enabled
            (
                SpanSlice {
                    service: "service",
                    name: "op",
                    resource: "res",
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("span.kind", "producer"),
                        ("aws.s3.bucket", "bucket-a"),
                        ("db.instance", "dynamo.test.us1"),
                        ("db.system", "dynamodb"),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "producer".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key_with_peers(vec![
                    ("aws.s3.bucket".into(), "bucket-a".into()),
                    ("db.instance".into(), "dynamo.test.us1".into()),
                    ("db.system".into(), "dynamodb".into()),
                ]),
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
                    meta: vec![
                        ("span.kind", "server"),
                        ("aws.s3.bucket", "bucket-a"),
                        ("db.instance", "dynamo.test.us1"),
                        ("db.system", "dynamodb"),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    span_kind: "server".into(),
                    is_trace_root: pb::Trilean::True,
                    ..Default::default()
                }
                .into_key(),
            ),
        ];

        for (span, expected_key) in test_cases {
            let borrowed_key = BorrowedAggregationKey::from_span(&span, &[], &[]);
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
            let borrowed_key =
                BorrowedAggregationKey::from_span(&span, test_peer_tags.as_slice(), &[]);
            assert_eq!(OwnedAggregationKey::from(&borrowed_key), expected_key);
            assert_eq!(
                get_hash(&borrowed_key),
                get_hash(&OwnedAggregationKey::from(&borrowed_key))
            );
        }
    }

    #[test]
    fn test_peer_tag_ip_quantization_in_aggregation_key() {
        let peer_tag_keys = vec!["peer.hostname".to_string(), "db.instance".to_string()];

        // IPv4 address peer tag gets replaced with blocked-ip-address
        let span_ipv4 = SpanSlice {
            service: "service",
            name: "op",
            resource: "res",
            span_id: 1,
            parent_id: 0,
            meta: vec![
                ("span.kind", "client"),
                ("peer.hostname", "10.1.2.3"),
                ("db.instance", "my-db"),
            ]
            .into(),
            ..Default::default()
        };
        let key = BorrowedAggregationKey::from_span(&span_ipv4, &peer_tag_keys, &[]);
        let owned = OwnedAggregationKey::from(&key);
        assert_eq!(
            owned.peer_tags,
            vec![
                (
                    "peer.hostname".to_string(),
                    "blocked-ip-address".to_string()
                ),
                ("db.instance".to_string(), "my-db".to_string()),
            ]
        );

        // IPv6 address peer tag gets replaced with blocked-ip-address
        let span_ipv6 = SpanSlice {
            service: "service",
            name: "op",
            resource: "res",
            span_id: 1,
            parent_id: 0,
            meta: vec![
                ("span.kind", "client"),
                ("peer.hostname", "2001:db8:3333:4444:CCCC:DDDD:EEEE:FFFF"),
            ]
            .into(),
            ..Default::default()
        };
        let ipv6_keys = vec!["peer.hostname".to_string()];
        let key = BorrowedAggregationKey::from_span(&span_ipv6, &ipv6_keys, &[]);
        let owned = OwnedAggregationKey::from(&key);
        assert_eq!(
            owned.peer_tags,
            vec![(
                "peer.hostname".to_string(),
                "blocked-ip-address".to_string()
            )]
        );

        // Non-IP peer tags pass through unchanged
        let span_non_ip = SpanSlice {
            service: "service",
            name: "op",
            resource: "res",
            span_id: 1,
            parent_id: 0,
            meta: vec![("span.kind", "client"), ("db.instance", "dynamo.test.us1")].into(),
            ..Default::default()
        };
        let non_ip_keys = vec!["db.instance".to_string()];
        let key = BorrowedAggregationKey::from_span(&span_non_ip, &non_ip_keys, &[]);
        let owned = OwnedAggregationKey::from(&key);
        assert_eq!(
            owned.peer_tags,
            vec![("db.instance".to_string(), "dynamo.test.us1".to_string())]
        );
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
