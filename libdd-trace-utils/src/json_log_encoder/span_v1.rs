// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serialization of a [`crate::span::v1::Span`] into the same Datadog Forwarder log exporter
//! JSON shape (APM v0.2 span schema) produced for v0.4 by [`super::span::LogSpan`].
//!
//! The wire contract is unchanged by the v1 migration — the Forwarder still expects
//! `meta`/`metrics`-shaped spans — only the input type is v1. So a v1 span's unified
//! `attributes` map is split back into `meta` (string/bool-valued) and `metrics`
//! (int/float-valued) the same way the [`crate::msgpack_encoder::v04::span_v1`] downgrade
//! encoder does for the msgpack wire; see that module's mapping table for the full
//! convention. `Bytes` attributes have no JSON log representation and are dropped (same as
//! `meta_struct` is never emitted for v0.4 — see [`super::span::LogSpan`]'s doc comment).
//!
//! Unlike the msgpack downgrade encoder, there is no payload-level `env`/`app_version`
//! fallback here: chunk-level context (`trace_id`, `origin`, `priority`,
//! `sampling_mechanism`, chunk attributes) is the only context propagated into each span,
//! matching the [`TraceChunk`]-level granularity at which the log-export path operates.

use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink, TraceChunk};
use crate::span::vec_map::DedupedVecMap;
use crate::span::{TraceData, SPAN_LINK_FLAGS_SET_SENTINEL};
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt::Write as _;

use super::span::{HexTraceId, HexU64};

/// Reserved v0.4 `meta`/`metrics` key names written from dedicated typed fields (`env`,
/// chunk `origin`, ...) rather than from the attribute map. See
/// [`crate::msgpack_encoder::v04::span_v1::PROMOTED_ATTR_KEYS`] for the msgpack-side sibling
/// of this list (this one omits `_dd.p.tid`: the JSON log's `trace_id` field carries the full
/// 128 bits directly, so there is no separate high-bits meta key here).
const PROMOTED_ATTR_KEYS: &[&str] = &[
    "env",
    "version",
    "component",
    "span.kind",
    "_dd.origin",
    "_dd.p.dm",
    "_sampling_priority_v1",
];

/// Chunk-level context propagated into every span when downgrading to the JSON log shape.
/// Built once per chunk and passed by reference to [`LogSpanV1`].
pub(super) struct ChunkContextV1<'a, T: TraceData> {
    pub trace_id: &'a [u8; 16],
    pub priority: Option<i32>,
    pub origin: &'a T::Text,
    pub sampling_mechanism: Option<u32>,
    pub attrs_dd: DedupedVecMap<'a, T::Text, AttributeValue<T>>,
}

impl<'a, T: TraceData> ChunkContextV1<'a, T> {
    pub(super) fn new(chunk: &'a TraceChunk<T>) -> Self {
        // v0.4 has no wire-level equivalent of `dropped_trace`; force `USER_REJECT` (-1)
        // unless the chunk already carries a negative (reject-like) priority — same
        // convention as the msgpack downgrade encoder
        // (`crate::msgpack_encoder::v04::encode_payload_from_v1`).
        let priority = if chunk.dropped_trace {
            Some(chunk.priority.filter(|&p| p < 0).unwrap_or(-1))
        } else {
            chunk.priority
        };
        Self {
            trace_id: &chunk.trace_id,
            priority,
            origin: &chunk.origin,
            sampling_mechanism: chunk.sampling_mechanism,
            attrs_dd: chunk.attributes.defensive_dedup(),
        }
    }
}

/// Maps a `SpanKind` to its v0.4 `span.kind` meta string. Returns `None` for `Internal` so
/// callers can skip emitting the default value.
fn span_kind_to_meta(kind: SpanKind) -> Option<&'static str> {
    match kind {
        SpanKind::Internal => None,
        SpanKind::Server => Some("server"),
        SpanKind::Client => Some("client"),
        SpanKind::Producer => Some("producer"),
        SpanKind::Consumer => Some("consumer"),
    }
}

/// Drops entries whose key was already seen, keeping the first occurrence. See
/// `crate::msgpack_encoder::v04::span_v1::dedup_first_wins` for why this is needed:
/// two distinct attributes can flatten to the same dotted key.
fn dedup_first_wins<V>(mut leaves: Vec<(String, V)>) -> Vec<(String, V)> {
    let keep: Vec<bool> = {
        let mut seen: HashSet<&str> = HashSet::with_capacity(leaves.len());
        leaves
            .iter()
            .map(|(k, _)| seen.insert(k.as_str()))
            .collect()
    };
    let mut keep = keep.into_iter();
    leaves.retain(|_| keep.next().unwrap_or(false));
    leaves
}

/// Recursively flattens a `List`/`KeyValue` attribute into dotted-key leaf entries for the
/// `meta` (string-valued) and `metrics` (numeric) buckets. Mirrors
/// `crate::msgpack_encoder::v04::span_v1::flatten_attr_into` exactly (same flattening
/// convention on both wire formats). `Bytes` has no flattened form and is dropped by the
/// caller before recursing.
fn flatten_attr_into<T: TraceData>(
    key: &mut String,
    v: &AttributeValue<T>,
    meta_out: &mut Vec<(String, String)>,
    metrics_out: &mut Vec<(String, f64)>,
) {
    match v {
        AttributeValue::String(s) => meta_out.push((key.clone(), s.borrow().to_owned())),
        AttributeValue::Bool(b) => {
            meta_out.push((key.clone(), if *b { "true" } else { "false" }.to_owned()))
        }
        AttributeValue::Int(i) => metrics_out.push((key.clone(), *i as f64)),
        AttributeValue::Float(f) => metrics_out.push((key.clone(), *f)),
        AttributeValue::Bytes(_) => {
            // Callers filter `Bytes` out before recursing; unreachable in practice.
        }
        AttributeValue::List(items) => {
            let base_len = key.len();
            for (i, item) in items.iter().enumerate() {
                key.push('.');
                let _ = write!(key, "{i}");
                flatten_attr_into(key, item, meta_out, metrics_out);
                key.truncate(base_len);
            }
        }
        AttributeValue::KeyValue(map) => {
            let base_len = key.len();
            for (k, v) in map.defensive_dedup().iter() {
                key.push('.');
                key.push_str(k.borrow());
                flatten_attr_into(key, v, meta_out, metrics_out);
                key.truncate(base_len);
            }
        }
    }
}

/// Serializes the `meta` object: promoted string fields, then flattened string/bool leaves.
struct LogMetaV1<'a> {
    env: &'a str,
    version: &'a str,
    component: &'a str,
    kind: Option<&'static str>,
    origin: &'a str,
    sampling_mechanism: Option<u32>,
    leaves: &'a [(String, String)],
}

impl Serialize for LogMetaV1<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let count = !self.env.is_empty() as usize
            + !self.version.is_empty() as usize
            + !self.component.is_empty() as usize
            + self.kind.is_some() as usize
            + !self.origin.is_empty() as usize
            + self.sampling_mechanism.is_some() as usize
            + self.leaves.len();

        let mut map = serializer.serialize_map(Some(count))?;
        if !self.env.is_empty() {
            map.serialize_entry("env", self.env)?;
        }
        if !self.version.is_empty() {
            map.serialize_entry("version", self.version)?;
        }
        if !self.component.is_empty() {
            map.serialize_entry("component", self.component)?;
        }
        if let Some(kind) = self.kind {
            map.serialize_entry("span.kind", kind)?;
        }
        if !self.origin.is_empty() {
            map.serialize_entry("_dd.origin", self.origin)?;
        }
        if let Some(mechanism) = self.sampling_mechanism {
            let mut buf = itoa::Buffer::new();
            map.serialize_entry("_dd.p.dm", buf.format(-(mechanism as i64)))?;
        }
        for (k, v) in self.leaves {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// Serializes the `metrics` object: the chunk's sampling priority, then flattened numeric
/// leaves.
struct LogMetricsV1<'a> {
    priority: Option<i32>,
    leaves: &'a [(String, f64)],
}

impl Serialize for LogMetricsV1<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let count = self.priority.is_some() as usize + self.leaves.len();
        let mut map = serializer.serialize_map(Some(count))?;
        if let Some(priority) = self.priority {
            map.serialize_entry("_sampling_priority_v1", &(priority as f64))?;
        }
        for (k, v) in self.leaves {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

/// Serializes a v1 [`SpanLink`]'s attributes: only `String`/`Bool` survive the downgrade to
/// the v0.4 `String -> String` link-attribute schema (`Bool` is stringified), same filter as
/// `crate::msgpack_encoder::v04::span_v1::encode_span_links`.
struct LogLinkAttrsV1<'a, T: TraceData>(&'a DedupedVecMap<'a, T::Text, AttributeValue<T>>);

impl<T: TraceData> Serialize for LogLinkAttrsV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entries: Vec<_> = self
            .0
            .iter()
            .filter(|(_, v)| matches!(v, AttributeValue::String(_) | AttributeValue::Bool(_)))
            .collect();
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (k, v) in entries {
            match v {
                AttributeValue::String(s) => map.serialize_entry(k, s)?,
                AttributeValue::Bool(b) => {
                    map.serialize_entry(k, if *b { "true" } else { "false" })?
                }
                _ => unreachable!("filtered above"),
            }
        }
        map.end()
    }
}

/// Splits a 128-bit big-endian trace id into big-endian `(low_64, high_64)` — the same split
/// v0.4's own `SpanLink` uses for its `trace_id`/`trace_id_high` fields.
fn split_trace_id(trace_id: &[u8; 16]) -> (u64, u64) {
    let mut high_bytes = [0u8; 8];
    let mut low_bytes = [0u8; 8];
    high_bytes.copy_from_slice(&trace_id[..8]);
    low_bytes.copy_from_slice(&trace_id[8..]);
    (
        u64::from_be_bytes(low_bytes),
        u64::from_be_bytes(high_bytes),
    )
}

/// Serializes a v1 [`SpanLink`] in the v0.4 JSON log shape without requiring `T: Serialize`
/// (only `T::Text`, via the `SpanText` bound, is needed) — same rationale as
/// [`super::span::LogSpanLink`].
struct LogSpanLinkV1<'a, T: TraceData>(&'a SpanLink<T>);

impl<T: TraceData> Serialize for LogSpanLinkV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let link = self.0;
        let (trace_id_low, trace_id_high) = split_trace_id(&link.trace_id);
        let attrs_dd = link.attributes.defensive_dedup();
        let has_attributes = attrs_dd
            .iter()
            .any(|(_, v)| matches!(v, AttributeValue::String(_) | AttributeValue::Bool(_)));
        let has_tracestate = !Borrow::<str>::borrow(&link.tracestate).is_empty();
        let has_flags = link.flags != 0;

        let mut len = 3; // trace_id, trace_id_high, span_id (always)
        len += has_attributes as usize;
        len += has_tracestate as usize;
        len += has_flags as usize;

        let mut state = serializer.serialize_struct("span_link", len)?;
        state.serialize_field("trace_id", &trace_id_low)?;
        state.serialize_field("trace_id_high", &trace_id_high)?;
        state.serialize_field("span_id", &link.span_id)?;
        if has_attributes {
            state.serialize_field("attributes", &LogLinkAttrsV1(&attrs_dd))?;
        }
        if has_tracestate {
            state.serialize_field("tracestate", &link.tracestate)?;
        }
        if has_flags {
            // Mask off the internal "explicitly set" sentinel (bit 31): this JSON log field
            // is consumer-facing and must not expose the internal wire encoding.
            state.serialize_field("flags", &(link.flags & !SPAN_LINK_FLAGS_SET_SENTINEL))?;
        }
        state.end()
    }
}

/// Returns `true` when `v` can be downgraded to a v0.4 event-attribute (scalar or scalar
/// list) — same filter as
/// `crate::msgpack_encoder::v04::span_v1::is_supported_event_attr`.
fn is_supported_event_attr<T: TraceData>(v: &AttributeValue<T>) -> bool {
    matches!(
        v,
        AttributeValue::String(_)
            | AttributeValue::Bool(_)
            | AttributeValue::Int(_)
            | AttributeValue::Float(_)
            | AttributeValue::List(_)
    )
}

/// Returns `true` when `v` is a scalar that fits in a v0.4 array element (no nesting).
fn is_scalar_array_elem<T: TraceData>(v: &AttributeValue<T>) -> bool {
    matches!(
        v,
        AttributeValue::String(_)
            | AttributeValue::Bool(_)
            | AttributeValue::Int(_)
            | AttributeValue::Float(_)
    )
}

/// Serializes a scalar v1 attribute as a v0.4 `AttributeArrayValue`
/// (`{"type": <u8>, "<kind>_value": ...}`), no `Array` variant since v0.4 array elements are
/// themselves scalar. Mirrors
/// `crate::msgpack_encoder::v04::span_v1::write_event_array_element`.
struct LogEventArrayElemV1<'a, T: TraceData>(&'a AttributeValue<T>);

impl<T: TraceData> Serialize for LogEventArrayElemV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("attr", 2)?;
        match self.0 {
            AttributeValue::String(s) => {
                state.serialize_field("type", &0u8)?;
                state.serialize_field("string_value", s)?;
            }
            AttributeValue::Bool(b) => {
                state.serialize_field("type", &1u8)?;
                state.serialize_field("bool_value", b)?;
            }
            AttributeValue::Int(i) => {
                state.serialize_field("type", &2u8)?;
                state.serialize_field("int_value", i)?;
            }
            AttributeValue::Float(f) => {
                state.serialize_field("type", &3u8)?;
                state.serialize_field("double_value", f)?;
            }
            _ => unreachable!("filtered by is_scalar_array_elem"),
        }
        state.end()
    }
}

/// Serializes the `array_value.values` array of a `List` attribute, filtering to scalar
/// elements only (nested structural entries have no v0.4 array-element equivalent).
struct LogEventArrayValueV1<'a, T: TraceData>(&'a [AttributeValue<T>]);

impl<T: TraceData> Serialize for LogEventArrayValueV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let scalars: Vec<_> = self.0.iter().filter(|e| is_scalar_array_elem(e)).collect();
        let mut seq = serializer.serialize_seq(Some(scalars.len()))?;
        for elem in scalars {
            seq.serialize_element(&LogEventArrayElemV1(elem))?;
        }
        seq.end()
    }
}

/// Serializes a v1 event attribute value in the v0.4 `{"type": <u8>, "<kind>_value": ...}`
/// shape. `List` produces `{"type": 4, "array_value": {"values": [...]}}`. Mirrors
/// `crate::msgpack_encoder::v04::span_v1::write_event_attr_value`.
struct LogEventAttrValueV1<'a, T: TraceData>(&'a AttributeValue<T>);

impl<T: TraceData> Serialize for LogEventAttrValueV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AttributeValue::List(items) => {
                let mut state = serializer.serialize_struct("attr", 2)?;
                state.serialize_field("type", &4u8)?;
                state.serialize_field("array_value", &LogEventArrayValueSeq(items))?;
                state.end()
            }
            other => LogEventArrayElemV1(other).serialize(serializer),
        }
    }
}

/// The `{"values": [...]}` wrapper object around a `List`'s scalar elements.
struct LogEventArrayValueSeq<'a, T: TraceData>(&'a [AttributeValue<T>]);

impl<T: TraceData> Serialize for LogEventArrayValueSeq<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("array_value", 1)?;
        state.serialize_field("values", &LogEventArrayValueV1(self.0))?;
        state.end()
    }
}

/// Serializes a v1 [`SpanEvent`] in the v0.4 JSON log shape without requiring `T: Serialize`.
/// The event's attribute values (`AttributeValue<T>`, via [`LogEventAttrValueV1`]) already
/// serialize via a `T::Text`-bounded impl — same rationale as [`super::span::LogSpanEvent`].
struct LogSpanEventV1<'a, T: TraceData>(&'a SpanEvent<T>);

impl<T: TraceData> Serialize for LogSpanEventV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let event = self.0;
        let attrs_dd = event.attributes.defensive_dedup();
        let attr_count = attrs_dd
            .iter()
            .filter(|(_, v)| is_supported_event_attr(v))
            .count();
        let has_attributes = attr_count > 0;

        let mut len = 2; // time_unix_nano, name (always)
        len += has_attributes as usize;

        let mut state = serializer.serialize_struct("span_event", len)?;
        state.serialize_field("time_unix_nano", &event.time_unix_nano)?;
        state.serialize_field("name", &event.name)?;
        if has_attributes {
            state.serialize_field("attributes", &LogEventAttrsV1(&attrs_dd))?;
        }
        state.end()
    }
}

/// The `attributes` map of a [`SpanEvent`], filtered to supported variants.
struct LogEventAttrsV1<'a, T: TraceData>(&'a DedupedVecMap<'a, T::Text, AttributeValue<T>>);

impl<T: TraceData> Serialize for LogEventAttrsV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entries: Vec<_> = self
            .0
            .iter()
            .filter(|(_, v)| is_supported_event_attr(v))
            .collect();
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (k, v) in entries {
            map.serialize_entry(k, &LogEventAttrValueV1(v))?;
        }
        map.end()
    }
}

/// Serializes a slice of `Item` as a JSON array using the `Wrap` per-element wrapper,
/// avoiding a `T: Serialize` bound on the element type — same rationale as
/// [`super::span::LogSeq`].
struct LogSeqV1<'a, I, W>(&'a [I], fn(&'a I) -> W);

impl<I, W: Serialize> Serialize for LogSeqV1<'_, I, W> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&(self.1)(item))?;
        }
        seq.end()
    }
}

/// Wraps a v1 [`Span`] (with its chunk context) so that [`serde_json`] emits the Datadog
/// Forwarder log shape — the same external wire contract as [`super::span::LogSpan`], but
/// downgrading v1's unified attribute model back to `meta`/`metrics` at serialization time.
pub(crate) struct LogSpanV1<'a, T: TraceData>(pub &'a Span<T>, pub &'a ChunkContextV1<'a, T>);

impl<T: TraceData> Serialize for LogSpanV1<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let span = self.0;
        let chunk = self.1;

        let span_attrs_dd = span.attributes.defensive_dedup();
        // Precedence: span attributes override chunk attributes. Attributes sharing a name
        // with a "promoted" dedicated field are dropped so the dedicated field always wins
        // and each key is written at most once.
        let merged_attrs = span_attrs_dd
            .iter()
            .filter(|(k, _)| !PROMOTED_ATTR_KEYS.contains(&(*k).borrow()))
            .chain(chunk.attrs_dd.iter().filter(|(k, _)| {
                !PROMOTED_ATTR_KEYS.contains(&(*k).borrow())
                    && !span_attrs_dd.iter().any(|(k2, _)| k2 == *k)
            }));

        let mut meta_leaves: Vec<(String, String)> = Vec::new();
        let mut metrics_leaves: Vec<(String, f64)> = Vec::new();
        let mut key_buf = String::new();
        for (k, v) in merged_attrs {
            if matches!(v, AttributeValue::Bytes(_)) {
                // No JSON log representation; dropped (mirrors `meta_struct` never being
                // emitted for v0.4 — see `super::span::LogSpan`'s doc comment).
                continue;
            }
            key_buf.clear();
            key_buf.push_str(k.borrow());
            flatten_attr_into(&mut key_buf, v, &mut meta_leaves, &mut metrics_leaves);
        }
        let meta_leaves = dedup_first_wins(meta_leaves);
        let metrics_leaves = dedup_first_wins(metrics_leaves);

        let kind_meta = span_kind_to_meta(span.span_kind);
        let env: &str = span.env.borrow();
        let version: &str = span.version.borrow();
        let component: &str = span.component.borrow();
        let origin: &str = chunk.origin.borrow();

        let has_meta = !env.is_empty()
            || !version.is_empty()
            || !component.is_empty()
            || kind_meta.is_some()
            || !origin.is_empty()
            || chunk.sampling_mechanism.is_some()
            || !meta_leaves.is_empty();
        let has_metrics = chunk.priority.is_some() || !metrics_leaves.is_empty();
        let has_type = !Borrow::<str>::borrow(&span.r#type).is_empty();
        let has_links = !span.span_links.is_empty();
        let has_events = !span.span_events.is_empty();

        // Required: trace_id, span_id, parent_id, service, name, resource, error, start,
        // duration. `meta_struct` has no v1 equivalent to emit (only `Bytes` attributes
        // would map to it, and those are dropped above).
        let mut len = 9;
        len += has_type as usize;
        len += has_meta as usize;
        len += has_metrics as usize;
        len += has_links as usize;
        len += has_events as usize;

        let mut state = serializer.serialize_struct("span", len)?;
        state.serialize_field(
            "trace_id",
            &HexTraceId(u128::from_be_bytes(*chunk.trace_id)),
        )?;
        state.serialize_field("span_id", &HexU64(span.span_id))?;
        state.serialize_field("parent_id", &HexU64(span.parent_id))?;
        state.serialize_field("service", &span.service)?;
        state.serialize_field("name", &span.name)?;
        state.serialize_field("resource", &span.resource)?;
        if has_type {
            state.serialize_field("type", &span.r#type)?;
        }
        // v0.4's `error` is always emitted as an integer (0/1) on the JSON log wire, unlike
        // v1's own `bool` field — same "required, integer" contract as `super::span::LogSpan`.
        state.serialize_field("error", &(span.error as i32))?;
        state.serialize_field("start", &span.start)?;
        state.serialize_field("duration", &span.duration)?;
        if has_meta {
            state.serialize_field(
                "meta",
                &LogMetaV1 {
                    env,
                    version,
                    component,
                    kind: kind_meta,
                    origin,
                    sampling_mechanism: chunk.sampling_mechanism,
                    leaves: &meta_leaves,
                },
            )?;
        }
        if has_metrics {
            state.serialize_field(
                "metrics",
                &LogMetricsV1 {
                    priority: chunk.priority,
                    leaves: &metrics_leaves,
                },
            )?;
        }
        if has_links {
            state.serialize_field("span_links", &LogSeqV1(&span.span_links, LogSpanLinkV1))?;
        }
        if has_events {
            state.serialize_field("span_events", &LogSeqV1(&span.span_events, LogSpanEventV1))?;
        }
        state.end()
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the v1::Span → JSON log downgrade encoder. Each test encodes a small
    //! set of `TraceChunk`s via [`super::super::encode_traces_v1`] and parses the single
    //! emitted line as `serde_json::Value` to assert on the resulting shape.
    use super::super::encode_traces_v1;
    use crate::span::v1::{
        AttributeValue, AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes,
        TraceChunkBytes,
    };
    use crate::span::vec_map::VecMap;
    use libdd_tinybytes::BytesString;
    use serde_json::Value;

    const MAX: usize = 64 * 1024;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
    }

    fn minimal_chunk(trace_id: [u8; 16], span: SpanBytes) -> TraceChunkBytes {
        TraceChunkBytes {
            trace_id,
            spans: vec![span],
            ..Default::default()
        }
    }

    fn minimal_span() -> SpanBytes {
        SpanBytes {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            span_id: 1,
            start: 1_000,
            duration: 500,
            ..Default::default()
        }
    }

    /// Encodes `chunks` and parses the single emitted line back into a `serde_json::Value`,
    /// returning the first span of the first trace for convenience.
    fn encode_first_span(chunks: &[TraceChunkBytes]) -> Value {
        let mut out = Vec::new();
        let stats = encode_traces_v1(chunks, &mut out, MAX).expect("encode ok");
        assert_eq!(stats.spans_dropped, 0);
        let text = String::from_utf8(out).expect("utf8");
        let line = text.lines().next().expect("at least one line");
        let v: Value = serde_json::from_str(line).expect("valid json");
        v["traces"][0][0].clone()
    }

    #[test]
    fn basic_span_writes_required_fields() {
        let chunk = minimal_chunk([0u8; 16], minimal_span());
        let span = encode_first_span(&[chunk]);

        assert_eq!(span["service"], "svc");
        assert_eq!(span["name"], "op");
        assert_eq!(span["resource"], "res");
        assert_eq!(span["span_id"], "0000000000000001");
        assert_eq!(span["trace_id"], "0000000000000000");
        assert_eq!(span["start"], 1_000);
        assert_eq!(span["duration"], 500);
        // `error` is always present as an integer, even when false.
        assert_eq!(span["error"], 0);
        // Optional fields must be absent when their underlying value is zero/empty.
        assert_eq!(span["parent_id"], "0000000000000000");
        assert!(span.get("type").is_none());
        assert!(span.get("meta").is_none());
        assert!(span.get("metrics").is_none());
        assert!(span.get("span_links").is_none());
        assert!(span.get("span_events").is_none());
        assert!(
            span.get("meta_struct").is_none(),
            "meta_struct has no JSON log representation and must never be emitted"
        );
    }

    #[test]
    fn promoted_fields_are_copied_into_meta() {
        let span = SpanBytes {
            env: bs("prod"),
            version: bs("1.2.3"),
            component: bs("http"),
            span_kind: SpanKind::Server,
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let meta = &out["meta"];

        assert_eq!(meta["env"], "prod");
        assert_eq!(meta["version"], "1.2.3");
        assert_eq!(meta["component"], "http");
        assert_eq!(meta["span.kind"], "server");
    }

    #[test]
    fn span_kind_internal_is_not_emitted() {
        let chunk = minimal_chunk([0u8; 16], minimal_span());
        let out = encode_first_span(&[chunk]);
        assert!(out.get("meta").is_none());
    }

    #[test]
    fn attribute_sharing_a_promoted_key_name_is_dropped_in_favor_of_the_dedicated_field() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("env"), AttributeValue::String(bs("staging")));
        attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        let span = SpanBytes {
            env: bs("prod"),
            attributes: attrs,
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let meta = &out["meta"];

        // The dedicated `span.env` field wins; the colliding attribute is dropped rather than
        // producing a duplicate `"env"` key on the wire.
        assert_eq!(meta["env"], "prod");
        assert_eq!(meta["http.method"], "GET");
    }

    #[test]
    fn span_attributes_override_chunk_attributes_of_the_same_key() {
        let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        chunk_attrs.insert(bs("k"), AttributeValue::String(bs("from-chunk")));
        let mut span_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        span_attrs.insert(bs("k"), AttributeValue::String(bs("from-span")));

        let chunk = TraceChunkBytes {
            attributes: chunk_attrs,
            ..minimal_chunk(
                [0u8; 16],
                SpanBytes {
                    attributes: span_attrs,
                    ..minimal_span()
                },
            )
        };
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["meta"]["k"], "from-span");
    }

    #[test]
    fn chunk_attributes_propagate_to_every_span() {
        let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        chunk_attrs.insert(bs("shared"), AttributeValue::String(bs("chunk-value")));
        let chunk = TraceChunkBytes {
            attributes: chunk_attrs,
            spans: vec![
                SpanBytes {
                    span_id: 1,
                    ..minimal_span()
                },
                SpanBytes {
                    span_id: 2,
                    ..minimal_span()
                },
            ],
            ..Default::default()
        };

        let mut out = Vec::new();
        let stats = encode_traces_v1(&[chunk], &mut out, MAX).expect("encode ok");
        assert_eq!(stats.spans_written, 2);
        let text = String::from_utf8(out).expect("utf8");
        let v: Value = serde_json::from_str(text.lines().next().unwrap()).expect("valid json");
        assert_eq!(v["traces"][0][0]["meta"]["shared"], "chunk-value");
        assert_eq!(v["traces"][0][1]["meta"]["shared"], "chunk-value");
    }

    #[test]
    fn flattened_attribute_colliding_with_another_attribute_keeps_first_wins() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("a"),
            AttributeValue::List(vec![AttributeValue::String(bs("from-list"))]),
        );
        attrs.insert(bs("a.0"), AttributeValue::String(bs("from-literal")));
        attrs.dedup();
        let span = SpanBytes {
            attributes: attrs,
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        // Only one "a.0" entry can exist in a JSON object; this just asserts it's present and
        // that encoding didn't panic/duplicate-serialize.
        assert!(out["meta"]["a.0"].is_string());
    }

    #[test]
    fn trace_id_128_bit_is_rendered_as_32_hex_chars() {
        let mut tid = [0u8; 16];
        tid[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABE_u64.to_be_bytes());
        tid[8..].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_be_bytes());
        let chunk = minimal_chunk(tid, minimal_span());
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["trace_id"], "deadbeefcafebabe0123456789abcdef");
    }

    #[test]
    fn trace_id_high_zero_renders_as_16_hex_chars() {
        let mut tid = [0u8; 16];
        tid[8..].copy_from_slice(&42u64.to_be_bytes());
        let chunk = minimal_chunk(tid, minimal_span());
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["trace_id"], "000000000000002a");
    }

    #[test]
    fn error_true_emits_one_false_emits_zero() {
        let chunk_err = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                error: true,
                ..minimal_span()
            },
        );
        assert_eq!(encode_first_span(&[chunk_err])["error"], 1);

        let chunk_ok = minimal_chunk([0u8; 16], minimal_span());
        assert_eq!(encode_first_span(&[chunk_ok])["error"], 0);
    }

    #[test]
    fn string_attribute_is_routed_to_meta() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        let chunk = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["meta"]["http.method"], "GET");
    }

    #[test]
    fn bool_attribute_is_stringified_in_meta() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("retry"), AttributeValue::Bool(true));
        attrs.insert(bs("cached"), AttributeValue::Bool(false));
        let chunk = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["meta"]["retry"], "true");
        assert_eq!(out["meta"]["cached"], "false");
    }

    #[test]
    fn float_and_int_attributes_route_to_metrics_as_f64() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("duration_ms"), AttributeValue::Float(12.5));
        attrs.insert(bs("status"), AttributeValue::Int(200));
        let chunk = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["metrics"]["duration_ms"], 12.5);
        assert_eq!(out["metrics"]["status"], 200.0);
    }

    #[test]
    fn bytes_attribute_is_dropped() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("blob"),
            AttributeValue::Bytes(libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad")),
        );
        attrs.insert(bs("kept"), AttributeValue::String(bs("yes")));
        let chunk = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let out = encode_first_span(&[chunk]);
        assert!(out["meta"].get("blob").is_none());
        assert_eq!(out["meta"]["kept"], "yes");
    }

    #[test]
    fn nested_key_value_attribute_is_flattened_with_dotted_keys() {
        let mut inner: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        inner.insert(bs("b"), AttributeValue::String(bs("v")));
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("a"), AttributeValue::KeyValue(inner));
        let chunk = minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["meta"]["a.b"], "v");
    }

    #[test]
    fn chunk_origin_priority_and_sampling_mechanism_are_mapped() {
        let chunk = TraceChunkBytes {
            origin: bs("rum"),
            priority: Some(1),
            sampling_mechanism: Some(3),
            ..minimal_chunk([0u8; 16], minimal_span())
        };
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["meta"]["_dd.origin"], "rum");
        assert_eq!(out["meta"]["_dd.p.dm"], "-3");
        assert_eq!(out["metrics"]["_sampling_priority_v1"], 1.0);
    }

    #[test]
    fn dropped_trace_forces_user_reject_priority() {
        let chunk = TraceChunkBytes {
            dropped_trace: true,
            priority: Some(1),
            ..minimal_chunk([0u8; 16], minimal_span())
        };
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["metrics"]["_sampling_priority_v1"], -1.0);
    }

    #[test]
    fn dropped_trace_keeps_existing_negative_priority() {
        let chunk = TraceChunkBytes {
            dropped_trace: true,
            priority: Some(-2),
            ..minimal_chunk([0u8; 16], minimal_span())
        };
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["metrics"]["_sampling_priority_v1"], -2.0);
    }

    #[test]
    fn span_link_attributes_are_filtered_to_string_and_bool() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("kept"), AttributeValue::String(bs("v")));
        attrs.insert(bs("kept_bool"), AttributeValue::Bool(true));
        attrs.insert(bs("dropped"), AttributeValue::Int(1));
        let span = SpanBytes {
            span_links: thin_vec::thin_vec![SpanLinkBytes {
                trace_id: [0u8; 16],
                span_id: 7,
                attributes: attrs,
                ..Default::default()
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let link = &out["span_links"][0];
        assert_eq!(link["span_id"], 7);
        assert_eq!(link["attributes"]["kept"], "v");
        assert_eq!(link["attributes"]["kept_bool"], "true");
        assert!(link["attributes"].get("dropped").is_none());
    }

    #[test]
    fn span_link_trace_id_splits_into_low_and_high_plain_integers() {
        let mut tid = [0u8; 16];
        tid[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABE_u64.to_be_bytes());
        tid[8..].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_be_bytes());
        let span = SpanBytes {
            span_links: thin_vec::thin_vec![SpanLinkBytes {
                trace_id: tid,
                span_id: 7,
                ..Default::default()
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let link = &out["span_links"][0];
        assert_eq!(link["trace_id"], 0x0123_4567_89AB_CDEF_u64);
        assert_eq!(link["trace_id_high"], 0xDEAD_BEEF_CAFE_BABE_u64);
    }

    #[test]
    fn span_link_flags_mask_off_set_sentinel_bit() {
        let span = SpanBytes {
            span_links: thin_vec::thin_vec![SpanLinkBytes {
                trace_id: [0u8; 16],
                span_id: 7,
                flags: crate::span::SPAN_LINK_FLAGS_SET_SENTINEL | 0b1,
                ..Default::default()
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        assert_eq!(out["span_links"][0]["flags"], 0b1);
    }

    #[test]
    fn span_event_scalar_attributes_use_typed_any_value_shape() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("s"), AttributeValue::String(bs("v")));
        attrs.insert(bs("b"), AttributeValue::Bool(true));
        attrs.insert(bs("i"), AttributeValue::Int(1));
        attrs.insert(bs("f"), AttributeValue::Float(1.5));
        let span = SpanBytes {
            span_events: thin_vec::thin_vec![SpanEventBytes {
                time_unix_nano: 123,
                name: bs("evt"),
                attributes: attrs,
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let event = &out["span_events"][0];
        assert_eq!(event["time_unix_nano"], 123);
        assert_eq!(event["name"], "evt");
        assert_eq!(
            event["attributes"]["s"],
            serde_json::json!({"type": 0, "string_value": "v"})
        );
        assert_eq!(
            event["attributes"]["b"],
            serde_json::json!({"type": 1, "bool_value": true})
        );
        assert_eq!(
            event["attributes"]["i"],
            serde_json::json!({"type": 2, "int_value": 1})
        );
        assert_eq!(
            event["attributes"]["f"],
            serde_json::json!({"type": 3, "double_value": 1.5})
        );
    }

    #[test]
    fn span_event_list_attribute_becomes_array_value_of_scalars() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("list"),
            AttributeValue::List(vec![
                AttributeValue::String(bs("a")),
                AttributeValue::Int(2),
            ]),
        );
        let span = SpanBytes {
            span_events: thin_vec::thin_vec![SpanEventBytes {
                time_unix_nano: 1,
                name: bs("evt"),
                attributes: attrs,
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        let value = &out["span_events"][0]["attributes"]["list"];
        assert_eq!(value["type"], 4);
        assert_eq!(
            value["array_value"]["values"],
            serde_json::json!([
                {"type": 0, "string_value": "a"},
                {"type": 2, "int_value": 2}
            ])
        );
    }

    #[test]
    fn span_event_bytes_attribute_is_dropped() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("blob"),
            AttributeValue::Bytes(libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad")),
        );
        let span = SpanBytes {
            span_events: thin_vec::thin_vec![SpanEventBytes {
                time_unix_nano: 1,
                name: bs("evt"),
                attributes: attrs,
            }],
            ..minimal_span()
        };
        let chunk = minimal_chunk([0u8; 16], span);
        let out = encode_first_span(&[chunk]);
        assert!(out["span_events"][0].get("attributes").is_none());
    }
}
