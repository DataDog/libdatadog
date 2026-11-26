// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implement the logic for stats aggregation into time buckets and stats group.
//! This includes the aggregation key to group spans together and the computation of stats from a
//! span.

use hashbrown::{HashMap, HashSet};
use libdd_trace_obfuscation::ip_address::quantize_peer_ip_addresses;
use libdd_trace_protobuf::pb;
use std::{
    borrow::{Borrow, Cow},
    hash::{DefaultHasher, Hash, Hasher as _},
    ops::Range,
};
use tracing::warn;

use crate::span_concentrator::cardinality_limit_telemetry::CollapsedFieldSet;
use libdd_trace_model::{Attributes as _, ChunkSpanView, Span, TraceChunk};
use libdd_trace_semantics::{Concept, Value, lookup_i64, lookup_str, lookup_value};

use super::{
    CardinalityLimitConfig,
    cardinality_limit_telemetry::{self, CollapsedFieldsMetrics},
};

/// Sentinel value used for cardinality limiting.
pub const TRACER_BLOCKED_VALUE: &str = "tracer_blocked_value";

/// Maximum number of characters of an additional metric tag value, longer values are replaced by
/// [`TRACER_BLOCKED_VALUE`].
pub const ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN: usize = 200;
const TAG_SYNTHETICS: &str = "synthetics";
const TAG_SVC_SRC: &str = "_dd.svc_src";

/// Aggregation key fields shared across all concentrator implementations — everything
/// **except** peer tags.
///
/// `T` is the string representation:
/// * `Cow<'a, str>` — borrowed/copy-on-write values used in `BorrowedAggregationKey`
/// * `&'a str`      — borrowed references (e.g. in `libdd-ipc`)
/// * `String`       — owned values used in `OwnedAggregationKey`
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
    fixed: FixedAggregationKey<Cow<'a, str>>,
    peer_tags: Vec<(&'a str, Cow<'a, str>)>,
    additional_metric_tags: Vec<(&'a str, &'a str)>,
}

impl hashbrown::Equivalent<OwnedAggregationKey> for BorrowedAggregationKey<'_> {
    #[inline]
    fn equivalent(&self, other: &OwnedAggregationKey) -> bool {
        self.fixed.convert::<&str, str, _>(|s| s) == other.fixed.convert(|s| s)
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

/// Return true if `value` has more than [`ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN`] characters.
pub fn additional_metric_tag_value_too_long(value: &str) -> bool {
    // Byte length >= char count, so skip the char walk when byte length alone is within the max
    // character length, otherwise stop as soon as we pass the max character length.
    value.len() > ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN
        && value
            .chars()
            .nth(ADDITIONAL_METRIC_TAG_VALUE_MAX_LEN)
            .is_some()
}

const VALID_HTTP_STATUS_CODES: Range<i64> = 100..600;

/// The HTTP status code from the trace semantics registry: the first value readable as an integer,
/// or 0 when there is none or it is outside `100..=599`.
///
/// Deliberately stricter than the Agent's `getStatusCode` (which keeps `0..=u32::MAX`), since valid
/// HTTP status codes are `100..=599` (RFC 9110). The Agent's normalizer uses the same range, but
/// it deletes an invalid `http.status_code` meta string, so the Agent can fall through to the OTel
/// name where this returns 0.
fn get_http_status_code(span: &impl Span) -> u32 {
    lookup_i64(span, Concept::HttpStatusCode)
        .filter(|v| VALID_HTTP_STATUS_CODES.contains(v))
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or_default()
}

/// The gRPC status code from the trace semantics registry.
///
/// The first non-empty value found wins. Non-integer numbers are skipped like missing values, as
/// `int64` entries read [`libdd_trace_model::Attributes::attribute_i64`]. Strings are parsed as a
/// number or a status name; an unparseable string or an integer outside the `u8` range yields
/// `None` without falling through.
fn get_grpc_status_code(span: &impl Span) -> Option<u8> {
    match lookup_value(span, Concept::RpcGrpcStatusCode)? {
        Value::String(s) => grpc_status_str_to_int_value(s),
        Value::Int(i) => u8::try_from(i).ok(),
        Value::Float(f) => {
            libdd_trace_model::f64_to_exact_i64(f).and_then(|i| u8::try_from(i).ok())
        }
        _ => None,
    }
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
    pub(super) fn from_span<C: TraceChunk>(
        span: &'a ChunkSpanView<'a, C>,
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

    pub(crate) fn from_obfuscated_span<'b, C>(
        resource_name: &'a str,
        span: &'b ChunkSpanView<'b, C>,
        peer_tag_keys: &'b [String],
        additional_metric_tag_keys: &'b [String],
    ) -> BorrowedAggregationKey<'a>
    where
        C: TraceChunk,
        // resource_name is a temporary string on the stack the span will outlive it
        'b: 'a,
    {
        // Peer tag and additional metric tag keys are user-configured v0.4 tag names, which may
        // name a promoted tag.
        let compat = span.compat();
        let span_kind = span.span_kind().unwrap_or_default();
        // Mirrors the Go trace-agent's peerTagKeysToAggregateForSpan: peer tags are collected
        // only when peer-tag aggregation is configured, and then either from the configured
        // keys (client/producer/consumer spans) or, for internal spans (or spans without a
        // kind) carrying a non-empty _dd.base_service override, from _dd.base_service alone.
        // All other kinds, including server, get no peer tags.
        let peer_tags = if peer_tag_keys.is_empty() {
            // Peer-tag aggregation is disabled: no span gets peer tags, even when
            // _dd.base_service is set.
            vec![]
        } else if should_track_peer_tags(span_kind) {
            // Parse the meta tags of the span and return a list of the peer tags based on the list
            // of `peer_tag_keys`. IP address values are quantized to reduce cardinality.
            peer_tag_keys
                .iter()
                .filter_map(|key| {
                    let value = compat.attribute_str(key.as_str())?;
                    Some((key.as_str(), quantize_peer_ip_addresses(value)))
                })
                .collect()
        } else if (span_kind.is_empty() || span_kind.eq_ignore_ascii_case("internal"))
            && let Some(base_service) = lookup_str(span, Concept::DdBaseService)
        {
            // Internal spans with a base service override use only _dd.base_service as peer tag
            vec![(Concept::DdBaseService.name(), base_service)]
        } else {
            vec![]
        };

        let http_method = lookup_str(span, Concept::HttpMethod).unwrap_or_default();

        let http_endpoint = span
            .attribute_str("http.endpoint")
            .filter(|value| !value.is_empty())
            .map(Cow::Borrowed)
            .or_else(|| lookup_str(span, Concept::HttpRoute))
            .unwrap_or_default();

        let status_code = get_http_status_code(span);

        let grpc_status_code = get_grpc_status_code(span);
        let service_source = span.attribute_str(TAG_SVC_SRC).unwrap_or_default();

        let additional_metric_tags: Vec<(&'a str, &'a str)> = additional_metric_tag_keys
            .iter()
            .filter_map(|key| match compat.attribute_str(key.as_str()) {
                Some(v) if !v.is_empty() => {
                    if additional_metric_tag_value_too_long(v) {
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
                resource_name: Cow::Borrowed(resource_name),
                service_name: Cow::Borrowed(span.service()),
                operation_name: Cow::Borrowed(span.name()),
                span_type: Cow::Borrowed(span.typ()),
                span_kind: Cow::Borrowed(span_kind),
                http_method,
                http_endpoint,
                service_source: Cow::Borrowed(service_source),
                http_status_code: status_code,
                grpc_status_code,
                is_synthetics_request: span.chunk.origin().starts_with(TAG_SYNTHETICS),
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

    /// Truncates string fields in accordance with the cardinality limit RFC
    ///
    /// This should be called only after obfuscation
    #[cfg_attr(not(feature = "stats-obfuscation"), allow(unused))]
    pub(crate) fn truncate(&mut self, big_resource: bool) {
        let resource_length_limit = if big_resource { 15_000 } else { 5000 };
        truncate_cow(&mut self.fixed.resource_name, resource_length_limit);
        truncate_cow(&mut self.fixed.service_name, 100);
        truncate_cow(&mut self.fixed.operation_name, 100);
        truncate_cow(&mut self.fixed.span_type, 100);
    }
}

/// Truncate `s` to at most `max_len` bytes in place
fn truncate_cow(s: &mut Cow<'_, str>, max_len: usize) {
    match s {
        Cow::Borrowed(b) => *b = slice_up_to(b, max_len),
        Cow::Owned(o) => {
            let new_len = slice_up_to(o, max_len).len();
            o.truncate(new_len);
        }
    }
}

/// Truncate `s` to at most `max_len` bytes
fn slice_up_to(s: &str, max_len: usize) -> &str {
    if max_len >= s.len() {
        return s;
    }
    // TODO: use `floor_char_boundary` once our MSRV is 1.91 or higher
    // (https://doc.rust-lang.org/std/primitive.str.html#method.floor_char_boundary)
    let mut idx = max_len;
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
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
fn should_track_peer_tags(span_kind: &str) -> bool {
    matches!(
        span_kind.to_lowercase().as_str(),
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
            collapsed_fields_metrics: cardinality_limit_telemetry::CollapsedFieldsMetrics::zero(),
        }
    }

    /// Returns metrics on spans field collapse with reasons.
    pub fn collapsed_fields_metrics(&self) -> cardinality_limit_telemetry::CollapsedFieldsMetrics {
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
                e.insert_with_key(OwnedAggregationKey::from(&key), GroupedStats::default())
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

        let mut collapsed_fields = CollapsedFieldSet::empty();

        let resource_hash = hash(&key.fixed.resource_name);
        let resources_count = self.distinct_resources.len();
        if let Entry::Vacant(slot) = self.distinct_resources.entry(resource_hash) {
            if resources_count >= self.cardinality_limits.resource_limit {
                key.fixed.resource_name = Cow::Borrowed(TRACER_BLOCKED_VALUE);
                collapsed_fields.add(CollapsedFieldSet::RESOURCE_NAME);
            } else {
                slot.insert();
            }
        }

        let http_endpoint_hash = hash(&key.fixed.http_endpoint);
        let http_endpoints_count = self.distinct_http_endpoints.len();
        if let Entry::Vacant(slot) = self.distinct_http_endpoints.entry(http_endpoint_hash) {
            if http_endpoints_count >= self.cardinality_limits.http_endpoint_limit {
                key.fixed.http_endpoint = Cow::Borrowed(TRACER_BLOCKED_VALUE);
                collapsed_fields.add(CollapsedFieldSet::HTTP_ENDPOINT);
            } else {
                slot.insert();
            }
        }

        let peer_tags_hash = hash(&key.peer_tags);
        let peer_tags_count = self.distinct_peer_tags.len();
        if let Entry::Vacant(slot) = self.distinct_peer_tags.entry(peer_tags_hash) {
            if peer_tags_count >= self.cardinality_limits.peer_tags_limit {
                key.peer_tags = vec![(TRACER_BLOCKED_VALUE, Cow::Borrowed(""))];
                collapsed_fields.add(CollapsedFieldSet::PEER_TAGS);
            } else {
                slot.insert();
            }
        }

        let additional_tags_hash = hash(&key.additional_metric_tags);
        let additional_tags_count = self.distinct_additional_tags.len();
        if let Entry::Vacant(slot) = self.distinct_additional_tags.entry(additional_tags_hash) {
            if additional_tags_count >= self.cardinality_limits.additional_tags_limit {
                key.additional_metric_tags = vec![(TRACER_BLOCKED_VALUE, "")];
                collapsed_fields.add(CollapsedFieldSet::ADDITIONAL_TAGS);
            } else {
                slot.insert();
            }
        }
        self.collapsed_fields_metrics.increment(collapsed_fields);
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
    use libdd_trace_utils::span::v04::{self, SpanBytes, SpanSlice};
    use libdd_trace_utils::span::{RootSpanChunk, TraceData};

    use super::*;
    use libdd_trace_protobuf::pb;
    use std::hash::Hash;

    /// Builds the aggregation key of `view`, checking that the borrowed and owned keys hash alike
    /// and are equivalent.
    fn key_of<C: TraceChunk>(
        view: &ChunkSpanView<'_, C>,
        peer_tag_keys: &[String],
    ) -> OwnedAggregationKey {
        let borrowed = BorrowedAggregationKey::from_span(view, peer_tag_keys, &[]);
        let owned = OwnedAggregationKey::from(&borrowed);
        assert_eq!(
            get_hash(&borrowed),
            get_hash(&owned),
            "borrowed/owned hash mismatch"
        );
        assert!(
            hashbrown::Equivalent::equivalent(&borrowed, &owned),
            "borrowed/owned equivalence mismatch"
        );
        owned
    }

    /// [`key_of`] for a v0.4 span, wrapped as a single-span chunk.
    fn v04_key_of<T: TraceData>(
        span: &v04::Span<T>,
        peer_tag_keys: &[String],
    ) -> OwnedAggregationKey {
        let chunk = RootSpanChunk::new(std::slice::from_ref(span));
        key_of(
            &ChunkSpanView {
                span,
                chunk: &chunk,
            },
            peer_tag_keys,
        )
    }

    fn owned_pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

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
            // NaN Datadog status code falls through to the next registry entry.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.response.status_code".into(), "418".into())].into(),
                    metrics: vec![("http.status_code".into(), f64::NAN)].into(),
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
            // Out-of-range Datadog status code yields 0 without falling through to OTel.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.response.status_code".into(), "418".into())].into(),
                    metrics: vec![("http.status_code".into(), 600.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 0,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Fractional Datadog status code falls through to the next registry entry.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.response.status_code".into(), "418".into())].into(),
                    metrics: vec![("http.status_code".into(), 1.5)].into(),
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
            // Out-of-range Datadog status code in meta yields 0 without falling through to OTel in
            // metrics.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.status_code".into(), "99".into())].into(),
                    metrics: vec![("http.response.status_code".into(), 418.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 0,
                    ..Default::default()
                }
                .into_key(),
            ),
            // OTel status codes outside 100..600 yield 0.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: vec![("http.response.status_code".into(), 600.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 0,
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
            // Span in OTel semantics mode: status code in metrics under the OTel name, which is
            // where the int-typed OTel attribute lands
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    metrics: vec![("http.response.status_code".into(), 418.0)].into(),
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
            // Span in OTel semantics mode: status code in meta under the OTel name, which is
            // where it lands whenever the tracer stringifies its attributes
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.response.status_code".into(), "418".into())].into(),
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
            // Datadog name in meta, OTel name in metrics: the Datadog name still wins, so the
            // metrics-before-meta order inside one name never leaks across names
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.status_code".into(), "500".into())].into(),
                    metrics: vec![("http.response.status_code".into(), 418.0)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 500,
                    ..Default::default()
                }
                .into_key(),
            ),
            // Non-numeric Datadog status in meta falls through to the next registry entry.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.status_code".into(), "x".into()),
                        ("http.response.status_code".into(), "418".into()),
                    ]
                    .into(),
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
            // NaN in Datadog metrics falls through to the string entry for the same key.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![("http.status_code".into(), "500".into())].into(),
                    metrics: vec![("http.status_code".into(), f64::NAN)].into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 500,
                    ..Default::default()
                }
                .into_key(),
            ),
            // An empty OTel method counts as absent, falling through to http.method.
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.request.method".into(), "".into()),
                        ("http.method".into(), "GET".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_method: "GET".into(),
                    ..Default::default()
                }
                .into_key(),
            ),
            // Both names present: registry order applies (Datadog wins for status code, OTel wins
            // for method).
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "res".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.status_code".into(), "500".into()),
                        ("http.response.status_code".into(), "418".into()),
                        ("http.method".into(), "GET".into()),
                        ("http.request.method".into(), "POST".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "res".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_status_code: 500,
                    http_method: "POST".into(),
                    ..Default::default()
                }
                .into_key(),
            ),
            // Span in OTel semantics mode: method under the OTel name, route unchanged because
            // http.route is already the OTel name
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "GET /api/v1/users".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.request.method".into(), "GET".into()),
                        ("http.route".into(), "/api/v1/users".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "GET /api/v1/users".into(),
                    is_synthetics_request: false,
                    is_trace_root: pb::Trilean::True,
                    http_method: "GET".into(),
                    http_endpoint: "/api/v1/users".into(),
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
            // Span with http.route and http.endpoint (http.endpoint takes precedence)
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "POST /users/create".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.method".into(), "POST".into()),
                        ("http.route".into(), "/users/create1".into()),
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
            // Span with empty http.endpoint and http.route (fallback to http.route)
            (
                SpanBytes {
                    service: "service".into(),
                    name: "op".into(),
                    resource: "POST /users/create".into(),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        ("http.method".into(), "POST".into()),
                        ("http.route".into(), "/users/create1".into()),
                        ("http.endpoint".into(), "".into()),
                    ]
                    .into(),
                    ..Default::default()
                },
                FixedAggregationKey {
                    service_name: "service".into(),
                    operation_name: "op".into(),
                    resource_name: "POST /users/create".into(),
                    http_method: "POST".into(),
                    http_endpoint: "/users/create1".into(),
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
            // Span with grpc status in meta takes precedence over metrics for the same key
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
                    service: Cow::Borrowed("service"),
                    name: Cow::Borrowed("op"),
                    resource: Cow::Borrowed("res"),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        (Cow::Borrowed("span.kind"), Cow::Borrowed("client")),
                        (Cow::Borrowed("aws.s3.bucket"), Cow::Borrowed("bucket-a")),
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
                .into_key_with_peers(vec![("aws.s3.bucket".into(), "bucket-a".into())]),
            ),
            // Span with multiple peer tags with peertags aggregation enabled
            (
                SpanSlice {
                    service: Cow::Borrowed("service"),
                    name: Cow::Borrowed("op"),
                    resource: Cow::Borrowed("res"),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        (Cow::Borrowed("span.kind"), Cow::Borrowed("producer")),
                        (Cow::Borrowed("aws.s3.bucket"), Cow::Borrowed("bucket-a")),
                        (
                            Cow::Borrowed("db.instance"),
                            Cow::Borrowed("dynamo.test.us1"),
                        ),
                        (Cow::Borrowed("db.system"), Cow::Borrowed("dynamodb")),
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
                    service: Cow::Borrowed("service"),
                    name: Cow::Borrowed("op"),
                    resource: Cow::Borrowed("res"),
                    span_id: 1,
                    parent_id: 0,
                    meta: vec![
                        (Cow::Borrowed("span.kind"), Cow::Borrowed("server")),
                        (Cow::Borrowed("aws.s3.bucket"), Cow::Borrowed("bucket-a")),
                        (
                            Cow::Borrowed("db.instance"),
                            Cow::Borrowed("dynamo.test.us1"),
                        ),
                        (Cow::Borrowed("db.system"), Cow::Borrowed("dynamodb")),
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
            assert_eq!(v04_key_of(&span, &[]), expected_key, "for span {span:?}");
        }

        for (span, expected_key) in test_cases_with_peer_tags {
            assert_eq!(v04_key_of(&span, &test_peer_tags), expected_key);
        }
    }

    /// Table-driven regression test for peer-tag selection in the aggregation key, mirroring
    /// the Go trace-agent's `peerTagKeysToAggregateForSpan`:
    /// - an empty configured key list suppresses all peer tags, including _dd.base_service;
    /// - client/producer/consumer spans use the configured keys;
    /// - spans with an absent, empty, or internal kind and a non-empty _dd.base_service use only
    ///   _dd.base_service;
    /// - server and unknown kinds get no peer tags, even when _dd.base_service is configured.
    #[test]
    fn test_base_service_peer_tag_selection_in_aggregation_key() {
        // (span.kind (None = absent), _dd.base_service (None = absent), configured
        // peer_tag_keys, extra meta, expected peer tags, expected stored span kind)
        type PeerTagSelectionCase = (
            Option<&'static str>,
            Option<&'static str>,
            Vec<&'static str>,
            Vec<(&'static str, &'static str)>,
            Vec<(&'static str, &'static str)>,
            &'static str,
        );
        let test_cases: Vec<PeerTagSelectionCase> = vec![
            // Absent kind with a non-empty base service: fallback applies.
            (
                None,
                Some("svc-a"),
                vec!["db.instance"],
                vec![("db.instance", "i-1")],
                vec![("_dd.base_service", "svc-a")],
                "",
            ),
            // Explicitly empty kind behaves like an absent kind.
            (
                Some(""),
                Some("svc-a"),
                vec!["db.instance"],
                vec![],
                vec![("_dd.base_service", "svc-a")],
                "",
            ),
            // Internal kind: only _dd.base_service, the other configured key is ignored.
            (
                Some("internal"),
                Some("svc-a"),
                vec!["db.instance"],
                vec![("db.instance", "i-1")],
                vec![("_dd.base_service", "svc-a")],
                "internal",
            ),
            // Mixed-case internal kind is selected case-insensitively but stored as-is.
            (
                Some("InTeRnAl"),
                Some("svc-a"),
                vec!["db.instance"],
                vec![("db.instance", "i-1")],
                vec![("_dd.base_service", "svc-a")],
                "InTeRnAl",
            ),
            // Empty base-service value does not trigger the fallback.
            (
                Some("internal"),
                Some(""),
                vec!["db.instance"],
                vec![],
                vec![],
                "internal",
            ),
            // Missing base service does not trigger the fallback.
            (
                Some("internal"),
                None,
                vec!["db.instance"],
                vec![],
                vec![],
                "internal",
            ),
            // Server kind never gets peer tags, even with a base service and configured keys
            // including _dd.base_service itself.
            (
                Some("server"),
                Some("svc-a"),
                vec!["db.instance", "_dd.base_service"],
                vec![("db.instance", "i-1")],
                vec![],
                "server",
            ),
            // Unknown kind never gets peer tags either.
            (
                Some("gateway"),
                Some("svc-a"),
                vec!["db.instance", "_dd.base_service"],
                vec![],
                vec![],
                "gateway",
            ),
            // An empty configured key list suppresses the base-service fallback.
            (
                Some("internal"),
                Some("svc-a"),
                vec![],
                vec![],
                vec![],
                "internal",
            ),
            // ...and also suppresses the client extraction.
            (
                Some("client"),
                Some("svc-a"),
                vec![],
                vec![("aws.s3.bucket", "bucket-a")],
                vec![],
                "client",
            ),
            // Client span with _dd.base_service itself configured: extracted via the
            // configured keys, in configured order.
            (
                Some("client"),
                Some("svc-a"),
                vec!["db.instance", "_dd.base_service"],
                vec![("db.instance", "i-1")],
                vec![("db.instance", "i-1"), ("_dd.base_service", "svc-a")],
                "client",
            ),
            // Same for producer and consumer.
            (
                Some("producer"),
                Some("svc-a"),
                vec!["_dd.base_service"],
                vec![],
                vec![("_dd.base_service", "svc-a")],
                "producer",
            ),
            (
                Some("consumer"),
                Some("svc-a"),
                vec!["_dd.base_service"],
                vec![],
                vec![("_dd.base_service", "svc-a")],
                "consumer",
            ),
            // Internal span with _dd.base_service configured: the fallback still uses only
            // _dd.base_service.
            (
                Some("internal"),
                Some("svc-a"),
                vec!["db.instance", "_dd.base_service"],
                vec![("db.instance", "i-1")],
                vec![("_dd.base_service", "svc-a")],
                "internal",
            ),
            // Mixed-case client kind is selected case-insensitively but stored as-is.
            (
                Some("CLIENT"),
                Some("svc-a"),
                vec!["db.instance"],
                vec![("db.instance", "i-1")],
                vec![("db.instance", "i-1")],
                "CLIENT",
            ),
        ];

        for (
            case_index,
            (kind, base_service, peer_tag_keys, extra_meta, expected_peer_tags, expected_kind),
        ) in test_cases.into_iter().enumerate()
        {
            let mut meta: Vec<(&str, &str)> = Vec::new();
            if let Some(kind) = kind {
                meta.push(("span.kind", kind));
            }
            if let Some(base_service) = base_service {
                meta.push(("_dd.base_service", base_service));
            }
            meta.extend(extra_meta);
            let span = SpanSlice {
                service: Cow::Borrowed("service"),
                name: Cow::Borrowed("op"),
                resource: Cow::Borrowed("res"),
                span_id: 1,
                parent_id: 0,
                meta: meta
                    .into_iter()
                    .map(|(k, v)| (Cow::Borrowed(k), Cow::Borrowed(v)))
                    .collect::<Vec<_>>()
                    .into(),
                ..Default::default()
            };
            let peer_tag_keys: Vec<String> =
                peer_tag_keys.iter().map(|key| key.to_string()).collect();
            let owned_key = v04_key_of(&span, &peer_tag_keys);
            assert_eq!(
                owned_key.peer_tags,
                owned_pairs(&expected_peer_tags),
                "unexpected peer tags for case {case_index}"
            );
            assert_eq!(
                owned_key.fixed.span_kind, expected_kind,
                "span kind must be stored with its original casing for case {case_index}"
            );
        }
    }

    /// V1 counterpart of `test_base_service_peer_tag_selection_in_aggregation_key`, covering what
    /// differs from v0.4: the kind is the dedicated `span_kind` field (`Internal` reads as unset),
    /// and both the kind and `_dd.base_service` fall back to the chunk's attributes.
    #[test]
    fn test_base_service_peer_tag_selection_v1() {
        use libdd_trace_utils::span::v1::{
            AttributeValue, SpanBytes as SpanBytesV1, SpanKind, TraceChunkBytes,
        };

        type Tags = &'static [(&'static str, &'static str)];
        // (span kind, span attributes, chunk attributes, expected peer tags, expected stored
        // span kind)
        let test_cases: &[(SpanKind, Tags, Tags, Tags, &str)] = &[
            // Internal kind reads as unset: the base-service fallback applies.
            (
                SpanKind::Internal,
                &[("_dd.base_service", "svc-a"), ("db.instance", "i-1")],
                &[],
                &[("_dd.base_service", "svc-a")],
                "",
            ),
            // The base service may come from the chunk...
            (
                SpanKind::Internal,
                &[],
                &[("_dd.base_service", "svc-chunk")],
                &[("_dd.base_service", "svc-chunk")],
                "",
            ),
            // ...but the span's own value wins.
            (
                SpanKind::Internal,
                &[("_dd.base_service", "svc-span")],
                &[("_dd.base_service", "svc-chunk")],
                &[("_dd.base_service", "svc-span")],
                "",
            ),
            // An unset span kind falls back to the chunk's, here selecting the configured keys.
            (
                SpanKind::Internal,
                &[("_dd.base_service", "svc-a"), ("db.instance", "i-1")],
                &[("span.kind", "client")],
                &[("db.instance", "i-1")],
                "client",
            ),
            // Server spans get no peer tags.
            (
                SpanKind::Server,
                &[("_dd.base_service", "svc-a"), ("db.instance", "i-1")],
                &[],
                &[],
                "server",
            ),
            // Client spans use the configured keys.
            (
                SpanKind::Client,
                &[("_dd.base_service", "svc-a"), ("db.instance", "i-1")],
                &[],
                &[("db.instance", "i-1")],
                "client",
            ),
        ];

        let attributes = |tags: Tags| {
            tags.iter()
                .map(|&(k, v)| (k.into(), AttributeValue::String(v.into())))
                .collect()
        };
        let peer_tag_keys = vec!["db.instance".to_string()];
        for (case_index, &(kind, span_tags, chunk_tags, expected_peer_tags, expected_kind)) in
            test_cases.iter().enumerate()
        {
            let chunk = TraceChunkBytes {
                spans: vec![SpanBytesV1 {
                    span_kind: kind,
                    attributes: attributes(span_tags),
                    ..Default::default()
                }],
                attributes: attributes(chunk_tags),
                ..Default::default()
            };
            let view = ChunkSpanView {
                span: &chunk.spans[0],
                chunk: &chunk,
            };
            let owned_key = key_of(&view, &peer_tag_keys);
            assert_eq!(
                owned_key.peer_tags,
                owned_pairs(expected_peer_tags),
                "unexpected peer tags for case {case_index}"
            );
            assert_eq!(
                owned_key.fixed.span_kind, expected_kind,
                "unexpected span kind for case {case_index}"
            );
        }
    }

    #[test]
    fn test_peer_tag_ip_quantization_in_aggregation_key() {
        let peer_tag_keys = vec!["peer.hostname".to_string(), "db.instance".to_string()];

        // IPv4 address peer tag gets replaced with blocked-ip-address
        let span_ipv4 = SpanSlice {
            service: Cow::Borrowed("service"),
            name: Cow::Borrowed("op"),
            resource: Cow::Borrowed("res"),
            span_id: 1,
            parent_id: 0,
            meta: vec![
                (Cow::Borrowed("span.kind"), Cow::Borrowed("client")),
                (Cow::Borrowed("peer.hostname"), Cow::Borrowed("10.1.2.3")),
                (Cow::Borrowed("db.instance"), Cow::Borrowed("my-db")),
            ]
            .into(),
            ..Default::default()
        };
        let owned = v04_key_of(&span_ipv4, &peer_tag_keys);
        assert_eq!(
            owned.peer_tags,
            owned_pairs(&[
                ("peer.hostname", "blocked-ip-address"),
                ("db.instance", "my-db"),
            ])
        );

        // IPv6 address peer tag gets replaced with blocked-ip-address
        let span_ipv6 = SpanSlice {
            service: Cow::Borrowed("service"),
            name: Cow::Borrowed("op"),
            resource: Cow::Borrowed("res"),
            span_id: 1,
            parent_id: 0,
            meta: vec![
                (Cow::Borrowed("span.kind"), Cow::Borrowed("client")),
                (
                    Cow::Borrowed("peer.hostname"),
                    Cow::Borrowed("2001:db8:3333:4444:CCCC:DDDD:EEEE:FFFF"),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let ipv6_keys = vec!["peer.hostname".to_string()];
        let owned = v04_key_of(&span_ipv6, &ipv6_keys);
        assert_eq!(
            owned.peer_tags,
            owned_pairs(&[("peer.hostname", "blocked-ip-address")])
        );

        // Non-IP peer tags pass through unchanged
        let span_non_ip = SpanSlice {
            service: Cow::Borrowed("service"),
            name: Cow::Borrowed("op"),
            resource: Cow::Borrowed("res"),
            span_id: 1,
            parent_id: 0,
            meta: vec![
                (Cow::Borrowed("span.kind"), Cow::Borrowed("client")),
                (
                    Cow::Borrowed("db.instance"),
                    Cow::Borrowed("dynamo.test.us1"),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let non_ip_keys = vec!["db.instance".to_string()];
        let owned = v04_key_of(&span_non_ip, &non_ip_keys);
        assert_eq!(
            owned.peer_tags,
            owned_pairs(&[("db.instance", "dynamo.test.us1")])
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

    fn v1_key_of(
        attrs: Vec<(
            &'static str,
            libdd_trace_utils::span::v1::AttributeValueBytes,
        )>,
    ) -> OwnedAggregationKey {
        use libdd_trace_utils::span::v1::{SpanBytes as SpanBytesV1, TraceChunkBytes};

        let chunk = TraceChunkBytes {
            spans: vec![SpanBytesV1 {
                attributes: attrs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let view = ChunkSpanView {
            span: &chunk.spans[0],
            chunk: &chunk,
        };
        key_of(&view, &[])
    }

    #[test]
    fn test_http_method_registry_precedence() {
        use libdd_trace_utils::span::v1::AttributeValue;

        // v0.4: OTel name wins when both are set.
        let both = SpanBytes {
            meta: vec![
                ("http.method".into(), "GET".into()),
                ("http.request.method".into(), "POST".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&both, &[]).fixed.http_method, "POST");

        // v0.4: empty OTel name falls through to http.method.
        let empty_otel = SpanBytes {
            meta: vec![
                ("http.request.method".into(), "".into()),
                ("http.method".into(), "DELETE".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&empty_otel, &[]).fixed.http_method, "DELETE");

        // V1: OTel name wins when both are set.
        assert_eq!(
            v1_key_of(vec![
                ("http.method", AttributeValue::String("GET".into())),
                ("http.request.method", AttributeValue::String("POST".into())),
            ])
            .fixed
            .http_method,
            "POST"
        );

        // V1: empty OTel name falls through to http.method.
        assert_eq!(
            v1_key_of(vec![
                ("http.request.method", AttributeValue::String("".into())),
                ("http.method", AttributeValue::String("PATCH".into())),
            ])
            .fixed
            .http_method,
            "PATCH"
        );
    }

    #[test]
    fn test_http_endpoint_precedence() {
        use libdd_trace_utils::span::v1::AttributeValue;

        // v0.4: http.endpoint wins over http.route, empty http.endpoint falls through.
        let both = SpanBytes {
            meta: vec![
                ("http.endpoint".into(), "/v1/items".into()),
                ("http.route".into(), "/v1/items/:id".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&both, &[]).fixed.http_endpoint, "/v1/items");

        let route_only = SpanBytes {
            meta: vec![("http.route".into(), "/v1/items/:id".into())].into(),
            ..Default::default()
        };
        assert_eq!(
            v04_key_of(&route_only, &[]).fixed.http_endpoint,
            "/v1/items/:id"
        );

        let empty_endpoint = SpanBytes {
            meta: vec![
                ("http.endpoint".into(), "".into()),
                ("http.route".into(), "/v1/items/:id".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(
            v04_key_of(&empty_endpoint, &[]).fixed.http_endpoint,
            "/v1/items/:id"
        );

        // V1: http.endpoint wins over http.route, empty http.endpoint falls through.
        assert_eq!(
            v1_key_of(vec![
                ("http.endpoint", AttributeValue::String("/v1/items".into())),
                ("http.route", AttributeValue::String("/v1/items/:id".into())),
            ])
            .fixed
            .http_endpoint,
            "/v1/items"
        );
        assert_eq!(
            v1_key_of(vec![
                ("http.endpoint", AttributeValue::String("".into())),
                ("http.route", AttributeValue::String("/v1/items/:id".into())),
            ])
            .fixed
            .http_endpoint,
            "/v1/items/:id"
        );
    }

    #[test]
    fn test_http_status_code_registry_behavior() {
        use libdd_trace_utils::span::v1::AttributeValue;

        // v0.4: valid HTTP status code boundaries (100 and 599) are kept for both names.
        for key in ["http.status_code", "http.response.status_code"] {
            for code in [100u32, 599] {
                let metric_span = SpanBytes {
                    metrics: vec![(key.into(), f64::from(code))].into(),
                    ..Default::default()
                };
                assert_eq!(
                    v04_key_of(&metric_span, &[]).fixed.http_status_code,
                    code,
                    "v0.4 metric {key}={code}"
                );
                let meta_span = SpanBytes {
                    meta: vec![(key.into(), code.to_string().into())].into(),
                    ..Default::default()
                };
                assert_eq!(
                    v04_key_of(&meta_span, &[]).fixed.http_status_code,
                    code,
                    "v0.4 meta {key}={code}"
                );
            }
        }

        // v0.4: status codes outside 100..600 yield 0 for both names.
        for key in ["http.status_code", "http.response.status_code"] {
            for code in [0u32, 99, 600, 999, u32::MAX] {
                let span = SpanBytes {
                    metrics: vec![(key.into(), f64::from(code))].into(),
                    ..Default::default()
                };
                assert_eq!(
                    v04_key_of(&span, &[]).fixed.http_status_code,
                    0,
                    "v0.4 metric {key}={code}"
                );
            }
        }

        // v0.4: an out-of-range metric yields 0 without falling through to the meta string under
        // the same name.
        let span = SpanBytes {
            metrics: vec![("http.response.status_code".into(), 600.0)].into(),
            meta: vec![("http.response.status_code".into(), "418".into())].into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&span, &[]).fixed.http_status_code, 0);

        // v0.4: out-of-range Datadog first values yield 0 without falling through to OTel.
        for invalid in [-1.0, 99.0, 600.0, 999.0, f64::from(u32::MAX) + 1.0] {
            let span = SpanBytes {
                metrics: vec![("http.status_code".into(), invalid)].into(),
                meta: vec![("http.response.status_code".into(), "418".into())].into(),
                ..Default::default()
            };
            assert_eq!(
                v04_key_of(&span, &[]).fixed.http_status_code,
                0,
                "v0.4 metric http.status_code={invalid} with valid OTel"
            );
        }

        // v0.4: out-of-range Datadog string values yield 0 without falling through to OTel.
        for invalid in ["-5", "99", "600", "999"] {
            let span = SpanBytes {
                meta: vec![
                    ("http.status_code".into(), invalid.into()),
                    ("http.response.status_code".into(), "418".into()),
                ]
                .into(),
                ..Default::default()
            };
            assert_eq!(
                v04_key_of(&span, &[]).fixed.http_status_code,
                0,
                "v0.4 meta http.status_code={invalid} with valid OTel"
            );
        }

        // v0.4: NaN / fractional / non-numeric Datadog values fall through to OTel.
        let dd_nan = SpanBytes {
            metrics: vec![("http.status_code".into(), f64::NAN)].into(),
            meta: vec![("http.response.status_code".into(), "418".into())].into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&dd_nan, &[]).fixed.http_status_code, 418);

        let dd_frac = SpanBytes {
            metrics: vec![("http.status_code".into(), 1.5)].into(),
            meta: vec![("http.response.status_code".into(), "418".into())].into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&dd_frac, &[]).fixed.http_status_code, 418);

        let dd_non_num = SpanBytes {
            meta: vec![
                ("http.status_code".into(), "bad".into()),
                ("http.response.status_code".into(), "418".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(v04_key_of(&dd_non_num, &[]).fixed.http_status_code, 418);

        // V1: valid HTTP status code boundaries (100 and 599) are kept for both names.
        for key in ["http.status_code", "http.response.status_code"] {
            for code in [100u32, 599] {
                assert_eq!(
                    v1_key_of(vec![(key, AttributeValue::Int(i64::from(code)))])
                        .fixed
                        .http_status_code,
                    code,
                    "V1 int {key}={code}"
                );
                assert_eq!(
                    v1_key_of(vec![(key, AttributeValue::String(code.to_string().into()))])
                        .fixed
                        .http_status_code,
                    code,
                    "V1 string {key}={code}"
                );
            }
        }

        // V1: status codes outside 100..600 yield 0 for both names.
        for key in ["http.status_code", "http.response.status_code"] {
            for code in [-1i64, 0, 99, 600, 999, i64::from(u32::MAX) + 1] {
                assert_eq!(
                    v1_key_of(vec![(key, AttributeValue::Int(code))])
                        .fixed
                        .http_status_code,
                    0,
                    "V1 int {key}={code}"
                );
            }
        }

        // V1: NaN / fractional / non-numeric Datadog values fall through to OTel.
        assert_eq!(
            v1_key_of(vec![
                ("http.status_code", AttributeValue::Float(f64::NAN)),
                ("http.response.status_code", AttributeValue::Int(418)),
            ])
            .fixed
            .http_status_code,
            418
        );
        assert_eq!(
            v1_key_of(vec![
                ("http.status_code", AttributeValue::Float(1.5)),
                ("http.response.status_code", AttributeValue::Int(418)),
            ])
            .fixed
            .http_status_code,
            418
        );
        assert_eq!(
            v1_key_of(vec![
                ("http.status_code", AttributeValue::String("bad".into())),
                ("http.response.status_code", AttributeValue::Int(418)),
            ])
            .fixed
            .http_status_code,
            418
        );

        // V1: out-of-range Datadog first values give 0 with no fall-through to OTel.
        for invalid in [-1i64, 99, 600, 999, i64::from(u32::MAX) + 1] {
            assert_eq!(
                v1_key_of(vec![
                    ("http.status_code", AttributeValue::Int(invalid)),
                    ("http.response.status_code", AttributeValue::Int(418)),
                ])
                .fixed
                .http_status_code,
                0,
                "V1 int http.status_code={invalid} with valid OTel"
            );
        }
        assert_eq!(
            v1_key_of(vec![
                ("http.status_code", AttributeValue::Float(999.0)),
                ("http.response.status_code", AttributeValue::Int(418)),
            ])
            .fixed
            .http_status_code,
            0
        );
    }

    #[test]
    fn test_grpc_status_code_registry_behavior() {
        use libdd_trace_utils::span::v1::AttributeValue;

        // v0.4: Registry order across names:
        // rpc.grpc.status_code > grpc.code > rpc.grpc.status.code > grpc.status.code >
        // rpc.response.status_code (when grpc)
        type Meta = &'static [(&'static str, &'static str)];
        let order_cases: &[(Meta, Option<u8>)] = &[
            (
                &[
                    ("rpc.grpc.status_code", "1"),
                    ("grpc.code", "2"),
                    ("rpc.grpc.status.code", "3"),
                    ("grpc.status.code", "4"),
                    ("rpc.system.name", "grpc"),
                    ("rpc.response.status_code", "5"),
                ],
                Some(1),
            ),
            (
                &[
                    ("grpc.code", "2"),
                    ("rpc.grpc.status.code", "3"),
                    ("grpc.status.code", "4"),
                    ("rpc.system.name", "grpc"),
                    ("rpc.response.status_code", "5"),
                ],
                Some(2),
            ),
            (
                &[
                    ("rpc.grpc.status.code", "3"),
                    ("grpc.status.code", "4"),
                    ("rpc.system.name", "grpc"),
                    ("rpc.response.status_code", "5"),
                ],
                Some(3),
            ),
            (
                &[
                    ("grpc.status.code", "4"),
                    ("rpc.system.name", "grpc"),
                    ("rpc.response.status_code", "5"),
                ],
                Some(4),
            ),
            // Unparseable first value gives None without falling through.
            (
                &[("rpc.grpc.status_code", "NOPE"), ("grpc.code", "OK")],
                None,
            ),
            // Empty first value is skipped and falls through.
            (
                &[("rpc.grpc.status_code", ""), ("grpc.code", "OK")],
                Some(0),
            ),
            // rpc.response.status_code with rpc.system.name == "grpc".
            (
                &[
                    ("rpc.system.name", "grpc"),
                    ("rpc.response.status_code", "UNAVAILABLE"),
                ],
                Some(14),
            ),
            // rpc.response.status_code with rpc.system == "grpc" and no rpc.system.name.
            (
                &[("rpc.system", "grpc"), ("rpc.response.status_code", "5")],
                Some(5),
            ),
            // rpc.response.status_code with rpc.system == "grpc" and empty rpc.system.name.
            (
                &[
                    ("rpc.system", "grpc"),
                    ("rpc.system.name", ""),
                    ("rpc.response.status_code", "5"),
                ],
                Some(5),
            ),
            // Ignored when rpc.system == "grpc" but rpc.system.name is set to another system.
            (
                &[
                    ("rpc.system", "grpc"),
                    ("rpc.system.name", "http"),
                    ("rpc.response.status_code", "5"),
                ],
                None,
            ),
            // Ignored when neither rpc.system.name nor rpc.system is "grpc".
            (&[("rpc.response.status_code", "5")], None),
            (
                &[
                    ("rpc.system.name", "connect_rpc"),
                    ("rpc.response.status_code", "5"),
                ],
                None,
            ),
        ];
        for (meta, expected) in order_cases {
            let span = SpanBytes {
                meta: meta
                    .iter()
                    .map(|&(k, v)| (k.into(), v.into()))
                    .collect::<Vec<_>>()
                    .into(),
                ..Default::default()
            };
            assert_eq!(
                v04_key_of(&span, &[]).fixed.grpc_status_code,
                *expected,
                "v0.4 meta={meta:?}"
            );
        }

        // v0.4: Numeric metric on an earlier name beats a string on a later name.
        let metric_earlier = SpanBytes {
            metrics: vec![("rpc.grpc.status_code".into(), 2.0)].into(),
            meta: vec![("grpc.code".into(), "OK".into())].into(),
            ..Default::default()
        };
        assert_eq!(
            v04_key_of(&metric_earlier, &[]).fixed.grpc_status_code,
            Some(2)
        );

        // v0.4: Out-of-range numeric metric gives None without falling through.
        let metric_out_of_range = SpanBytes {
            metrics: vec![("rpc.grpc.status_code".into(), 300.0)].into(),
            meta: vec![("grpc.code".into(), "OK".into())].into(),
            ..Default::default()
        };
        assert_eq!(
            v04_key_of(&metric_out_of_range, &[]).fixed.grpc_status_code,
            None
        );

        // v0.4: rpc.response.status_code as numeric metric when rpc.system.name == "grpc".
        let metric_otel = SpanBytes {
            metrics: vec![("rpc.response.status_code".into(), 13.0)].into(),
            meta: vec![("rpc.system.name".into(), "grpc".into())].into(),
            ..Default::default()
        };
        assert_eq!(
            v04_key_of(&metric_otel, &[]).fixed.grpc_status_code,
            Some(13)
        );

        // V1: Registry order, unparseable no-fallthrough, numeric earlier beats string later,
        // and rpc.response.status_code conditions.
        assert_eq!(
            v1_key_of(vec![
                ("rpc.grpc.status_code", AttributeValue::Int(2)),
                ("grpc.code", AttributeValue::String("OK".into())),
            ])
            .fixed
            .grpc_status_code,
            Some(2)
        );
        assert_eq!(
            v1_key_of(vec![
                ("rpc.grpc.status_code", AttributeValue::Int(-1)),
                ("grpc.code", AttributeValue::String("OK".into())),
            ])
            .fixed
            .grpc_status_code,
            None
        );
        assert_eq!(
            v1_key_of(vec![
                (
                    "rpc.grpc.status_code",
                    AttributeValue::String("NOPE".into())
                ),
                ("grpc.code", AttributeValue::String("OK".into())),
            ])
            .fixed
            .grpc_status_code,
            None
        );
        assert_eq!(
            v1_key_of(vec![
                ("rpc.system.name", AttributeValue::String("grpc".into())),
                ("rpc.response.status_code", AttributeValue::Int(14)),
            ])
            .fixed
            .grpc_status_code,
            Some(14)
        );
        assert_eq!(
            v1_key_of(vec![
                ("rpc.system", AttributeValue::String("grpc".into())),
                ("rpc.response.status_code", AttributeValue::Int(5)),
            ])
            .fixed
            .grpc_status_code,
            Some(5)
        );
        assert_eq!(
            v1_key_of(vec![
                ("rpc.system", AttributeValue::String("grpc".into())),
                ("rpc.system.name", AttributeValue::String("http".into())),
                ("rpc.response.status_code", AttributeValue::Int(5)),
            ])
            .fixed
            .grpc_status_code,
            None
        );
        assert_eq!(
            v1_key_of(vec![("rpc.response.status_code", AttributeValue::Int(5))])
                .fixed
                .grpc_status_code,
            None
        );
    }
}
