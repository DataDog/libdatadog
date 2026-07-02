// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Downgrade encoder: `crate::span::v1::Span` → v0.4 msgpack wire.
//! (Convention documented in [`crate::msgpack_encoder`].)
//!
//! Used when the receiving agent does not advertise the `/v1.0/traces` endpoint and the tracer
//! must fall back to v0.4. The mapping is:
//!
//! | v1::Span field / attribute            | v0.4 field                                  |
//! |---------------------------------------|---------------------------------------------|
//! | `env` / `version` / `component`       | `meta["env"]` / `meta["version"]` / ...  (`env`/`version` fall back to the payload-level `env`/`app_version` when unset on the span) |
//! | `span_kind`                           | `meta["span.kind"]` (lowercase string)      |
//! | `AttributeValue::String` / `Bool`     | `meta[k]` (`"true"` / `"false"` for bool)   |
//! | `AttributeValue::Float` / `Int`       | `metrics[k]` (Int cast to `f64`)            |
//! | `AttributeValue::Bytes`               | `meta_struct[k]` (raw bytes)                |
//! | `AttributeValue::List`                | flattened into `meta`/`metrics[k.0]`, `[k.1]`, ... (per element type) |
//! | `AttributeValue::KeyValue`            | flattened into `meta`/`metrics[k.a]`, `[k.a.b]`, ... (per member, recursively) |
//! | `error: bool`                         | `error: i32` (`true → 1`, `false → 0`)      |
//! | Chunk `trace_id: [u8; 16]`            | `trace_id: u64` (low 64) + `meta["_dd.p.tid"]` (hex of high 64, when non-zero) |
//! | Chunk `origin`                        | `meta["_dd.origin"]`                        |
//! | Chunk `priority`                      | `metrics["_sampling_priority_v1"]`          |
//! | Chunk `sampling_mechanism`            | `meta["_dd.p.dm"]` (`"-{mechanism}"`)       |
//! | Chunk `attributes`                    | Applied to every span in the chunk          |
//! | Payload `env` / `app_version`         | Fallback for `meta["env"]` / `meta["version"]` when the span leaves them unset |
//! | Payload `attributes`                  | Applied to every span, lowest precedence (span > chunk > payload) |
//! | Chunk `dropped_trace: true`           | Forces `metrics["_sampling_priority_v1"] = -1` (USER_REJECT) unless the chunk's own priority is already negative |
//!
//! An attribute sharing a name with one of the dedicated fields above (`env`, `version`,
//! `component`, `span.kind`, `_dd.p.tid`, `_dd.origin`, `_dd.p.dm`, `_sampling_priority_v1`) is
//! dropped: the dedicated field always wins, so each key is written at most once.

use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink};
use crate::span::vec_map::VecMap;
use crate::span::TraceData;
use rmp::encode::{
    write_array_len, write_bin, write_bool, write_f64, write_i64, write_map_len, write_sint,
    write_str, write_u32, write_u64, write_u8, RmpWrite, ValueWriteError,
};
use std::borrow::Borrow;
use std::collections::HashSet;

/// Writes a `bool` as the v0.4 string representation (`"true"` / `"false"`). Used wherever a
/// typed V1 `Bool` attribute is downgraded into v0.4 `meta` (which is `String → String` only).
fn write_bool_as_str<W: RmpWrite>(
    writer: &mut W,
    b: bool,
) -> Result<(), ValueWriteError<W::Error>> {
    write_str(writer, if b { "true" } else { "false" })
}

/// Reserved v0.4 `meta`/`metrics` key names written from dedicated typed fields (`span.env`,
/// chunk `origin`, ...) rather than from the attribute maps. An attribute sharing one of these
/// names would otherwise collide with the dedicated field's entry on the wire; the dedicated
/// field always wins and the same-named attribute is dropped — see `encode_span`.
const PROMOTED_ATTR_KEYS: &[&str] = &[
    "env",
    "version",
    "component",
    "span.kind",
    "_dd.p.tid",
    "_dd.origin",
    "_dd.p.dm",
    "_sampling_priority_v1",
];

/// Chunk-level context propagated into every span when downgrading to v0.4. Built once per
/// chunk by the top-level encoder and passed by reference to `encode_span_v1_to_v04`. Also
/// carries payload-level fields (`payload_env`, `payload_app_version`, `payload_attributes`),
/// which apply as a fallback when the span itself doesn't set the equivalent field — v0.4 has
/// neither a chunk nor a payload concept, so both levels collapse onto every span.
pub(super) struct ChunkContext<'a, T: TraceData> {
    pub trace_id: &'a [u8; 16],
    pub priority: Option<i32>,
    pub origin: &'a T::Text,
    pub sampling_mechanism: Option<u32>,
    pub attributes: &'a VecMap<T::Text, AttributeValue<T>>,
    pub payload_env: &'a T::Text,
    pub payload_app_version: &'a T::Text,
    pub payload_attributes: &'a VecMap<T::Text, AttributeValue<T>>,
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

/// Splits a 128-bit big-endian trace_id into big-endian `(low_64, high_64)`. The low half maps to
/// v0.4's `trace_id` field; the high half goes to `meta["_dd.p.tid"]` as hex when non-zero.
#[inline]
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

/// Per-bucket counts for the v0.4 `meta`, `metrics`, and `meta_struct` maps.
#[derive(Default)]
struct BucketCounts {
    meta: u32,
    metrics: u32,
    meta_struct: u32,
}

/// Recursively flattens a `List`/`KeyValue` attribute into dotted-key leaf entries for the v0.4
/// `meta` (string-valued) and `metrics` (numeric) maps — matching how intake/the UI expect nested
/// V1 attributes to be exploded: list elements become `key.0`, `key.1`, ... and `KeyValue`
/// members become `key.<member>` (recursively, for nested `KeyValue`/`List` values). Scalars
/// (`String`/`Bool`/`Int`/`Float`) are leaves in their own right and produce a single entry under
/// `key`. `Bytes` has no flattened form; callers must route it to `meta_struct` separately.
fn flatten_attr_into<T: TraceData>(
    key: String,
    v: &AttributeValue<T>,
    meta_out: &mut Vec<(String, String)>,
    metrics_out: &mut Vec<(String, f64)>,
) {
    match v {
        AttributeValue::String(s) => meta_out.push((key, s.borrow().to_owned())),
        AttributeValue::Bool(b) => {
            meta_out.push((key, if *b { "true" } else { "false" }.to_owned()))
        }
        AttributeValue::Int(i) => metrics_out.push((key, *i as f64)),
        AttributeValue::Float(f) => metrics_out.push((key, *f)),
        AttributeValue::Bytes(_) => {
            // Callers filter `Bytes` out before recursing; unreachable in practice.
        }
        AttributeValue::List(items) => {
            for (i, item) in items.iter().enumerate() {
                flatten_attr_into(format!("{key}.{i}"), item, meta_out, metrics_out);
            }
        }
        AttributeValue::KeyValue(map) => {
            for (k, v) in map.defensive_dedup().iter() {
                flatten_attr_into(format!("{key}.{}", k.borrow()), v, meta_out, metrics_out);
            }
        }
    }
}

/// Encodes a [`v1::Span`](crate::span::v1::Span) into the v0.4 msgpack wire format
/// (downgrade: v1 input → v0.4 output). Chunk-level context (`trace_id`, `origin`, `priority`,
/// `sampling_mechanism`, chunk attributes) is injected into the span's `meta` / `metrics` /
/// `meta_struct` maps since v0.4 has no chunk concept.
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `span` - The v1::Span to encode.
/// * `chunk` - Chunk-level context (`trace_id`, `origin`, `priority`, `sampling_mechanism`, chunk
///   attributes) propagated into the span on the v0.4 wire.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_span<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span: &Span<T>,
    chunk: &ChunkContext<'_, T>,
) -> Result<(), ValueWriteError<W::Error>> {
    let span_attrs_dd = span.attributes.defensive_dedup();
    let chunk_attrs_dd = chunk.attributes.defensive_dedup();
    let payload_attrs_dd = chunk.payload_attributes.defensive_dedup();

    // Merge span + chunk + payload attributes upfront with explicit "span overrides chunk
    // overrides payload" precedence. We don't rely on msgpack map last-write-wins decoding
    // here: the v0.4 / msgpack specs do not formalize behavior for duplicate map keys, so we
    // emit each key exactly once.
    //
    // Attributes sharing a name with a "promoted" dedicated field (`env`, `_dd.origin`, ...)
    // are dropped here: the dedicated field always wins so we never emit that key twice.
    let span_keys: HashSet<&T::Text> = span_attrs_dd.iter().map(|(k, _)| k).collect();
    let mut seen_keys: HashSet<&T::Text> = span_keys.clone();
    let chunk_only: Vec<(&T::Text, &AttributeValue<T>)> = chunk_attrs_dd
        .iter()
        .filter(|(k, _)| !seen_keys.contains(k))
        .collect();
    seen_keys.extend(chunk_attrs_dd.iter().map(|(k, _)| k));
    let payload_only: Vec<(&T::Text, &AttributeValue<T>)> = payload_attrs_dd
        .iter()
        .filter(|(k, _)| !seen_keys.contains(k))
        .collect();
    let merged_attrs: Vec<(&T::Text, &AttributeValue<T>)> = span_attrs_dd
        .iter()
        .chain(chunk_only)
        .chain(payload_only)
        .filter(|(k, _)| !PROMOTED_ATTR_KEYS.contains(&(*k).borrow()))
        .collect();

    let (trace_id_low, trace_id_high) = split_trace_id(chunk.trace_id);
    let kind_meta = span_kind_to_meta(span.span_kind);

    // `env`/`version` fall back to the payload-level value when the span doesn't set its own —
    // mirrors how a v1 tracer can set these once at the payload level instead of duplicating
    // them on every span/chunk.
    let env: &str = if !span.env.borrow().is_empty() {
        span.env.borrow()
    } else {
        chunk.payload_env.borrow()
    };
    let version: &str = if !span.version.borrow().is_empty() {
        span.version.borrow()
    } else {
        chunk.payload_app_version.borrow()
    };

    // Flatten every attribute into `meta` (string-valued) / `metrics` (numeric) leaf entries.
    // `List` and `KeyValue` have no v0.4 wire representation as a single value, so they are
    // exploded into dotted keys (`key.0`, `key.a.b`) the same way intake/the UI expect nested V1
    // attributes — see the mapping table in the module docs. `Bytes` keeps going to
    // `meta_struct` since it has no flattened form.
    let mut meta_leaves: Vec<(String, String)> = Vec::new();
    let mut metrics_leaves: Vec<(String, f64)> = Vec::new();
    let mut bytes_attrs: Vec<(&T::Text, &T::Bytes)> = Vec::new();
    for &(k, v) in &merged_attrs {
        match v {
            AttributeValue::Bytes(b) => bytes_attrs.push((k, b)),
            _ => flatten_attr_into(
                k.borrow().to_owned(),
                v,
                &mut meta_leaves,
                &mut metrics_leaves,
            ),
        }
    }

    // First pass: count bucket sizes so each msgpack map header carries the exact length.
    let mut counts = BucketCounts::default();
    counts.meta += !env.is_empty() as u32;
    counts.meta += !version.is_empty() as u32;
    counts.meta += !span.component.borrow().is_empty() as u32;
    counts.meta += kind_meta.is_some() as u32;
    counts.meta += (trace_id_high != 0) as u32;
    counts.meta += !chunk.origin.borrow().is_empty() as u32;
    counts.meta += chunk.sampling_mechanism.is_some() as u32;
    counts.meta += meta_leaves.len() as u32;
    counts.metrics += chunk.priority.is_some() as u32;
    counts.metrics += metrics_leaves.len() as u32;
    counts.meta_struct += bytes_attrs.len() as u32;

    let span_len = 7 // service, name, resource, trace_id, span_id, start, duration (always)
        + (!span.r#type.borrow().is_empty()) as u32
        + (span.parent_id != 0) as u32
        + span.error as u32
        + (counts.meta > 0) as u32
        + (counts.metrics > 0) as u32
        + (counts.meta_struct > 0) as u32
        + (!span.span_links.is_empty()) as u32
        + (!span.span_events.is_empty()) as u32;

    write_map_len(writer, span_len)?;

    write_const_msgpack_str!(writer, "service")?;
    write_str(writer, span.service.borrow())?;

    write_const_msgpack_str!(writer, "name")?;
    write_str(writer, span.name.borrow())?;

    write_const_msgpack_str!(writer, "resource")?;
    write_str(writer, span.resource.borrow())?;

    write_const_msgpack_str!(writer, "trace_id")?;
    write_u64(writer, trace_id_low)?;

    write_const_msgpack_str!(writer, "span_id")?;
    write_u64(writer, span.span_id)?;

    if span.parent_id != 0 {
        write_const_msgpack_str!(writer, "parent_id")?;
        write_u64(writer, span.parent_id)?;
    }

    write_const_msgpack_str!(writer, "start")?;
    write_i64(writer, span.start)?;

    write_const_msgpack_str!(writer, "duration")?;
    write_sint(writer, span.duration)?;

    if span.error {
        write_const_msgpack_str!(writer, "error")?;
        write_sint(writer, 1)?;
    }

    if counts.meta > 0 {
        write_const_msgpack_str!(writer, "meta")?;
        write_map_len(writer, counts.meta)?;

        if !env.is_empty() {
            write_const_msgpack_str!(writer, "env")?;
            write_str(writer, env)?;
        }
        if !version.is_empty() {
            write_const_msgpack_str!(writer, "version")?;
            write_str(writer, version)?;
        }
        if !span.component.borrow().is_empty() {
            write_const_msgpack_str!(writer, "component")?;
            write_str(writer, span.component.borrow())?;
        }
        if let Some(kind_str) = kind_meta {
            write_const_msgpack_str!(writer, "span.kind")?;
            write_str(writer, kind_str)?;
        }
        if trace_id_high != 0 {
            // Lower-case hex without `0x` prefix — the agent expects this format.
            write_const_msgpack_str!(writer, "_dd.p.tid")?;
            write_str(writer, &format!("{trace_id_high:016x}"))?;
        }
        if !chunk.origin.borrow().is_empty() {
            write_const_msgpack_str!(writer, "_dd.origin")?;
            write_str(writer, chunk.origin.borrow())?;
        }
        if let Some(mechanism) = chunk.sampling_mechanism {
            write_const_msgpack_str!(writer, "_dd.p.dm")?;
            write_str(writer, &format!("-{mechanism}"))?;
        }
        for (k, v) in &meta_leaves {
            write_str(writer, k)?;
            write_str(writer, v)?;
        }
    }

    if counts.metrics > 0 {
        write_const_msgpack_str!(writer, "metrics")?;
        write_map_len(writer, counts.metrics)?;

        if let Some(priority) = chunk.priority {
            write_const_msgpack_str!(writer, "_sampling_priority_v1")?;
            write_f64(writer, priority as f64)?;
        }
        for (k, v) in &metrics_leaves {
            write_str(writer, k)?;
            write_f64(writer, *v)?;
        }
    }

    if !span.r#type.borrow().is_empty() {
        write_const_msgpack_str!(writer, "type")?;
        write_str(writer, span.r#type.borrow())?;
    }

    if counts.meta_struct > 0 {
        write_const_msgpack_str!(writer, "meta_struct")?;
        write_map_len(writer, counts.meta_struct)?;

        for &(k, b) in &bytes_attrs {
            write_str(writer, k.borrow())?;
            write_bin(writer, b.borrow())?;
        }
    }

    if !span.span_links.is_empty() {
        encode_span_links(writer, &span.span_links)?;
    }
    if !span.span_events.is_empty() {
        encode_span_events(writer, &span.span_events)?;
    }

    Ok(())
}

/// Encodes [`v1::SpanLink`](crate::span::v1::SpanLink)s into the v0.4 msgpack wire format
/// (downgrade: v1 input → v0.4 output). The 128-bit `trace_id` is split into
/// `(trace_id, trace_id_high)` u64s. Typed link attributes are downgraded to strings;
/// non-string-coercible variants are dropped because v0.4 link attributes are `String → String`
/// only.
fn encode_span_links<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_links: &[SpanLink<T>],
) -> Result<(), ValueWriteError<W::Error>> {
    write_const_msgpack_str!(writer, "span_links")?;
    write_array_len(writer, span_links.len() as u32)?;

    for link in span_links {
        let (trace_id_low, trace_id_high) = split_trace_id(&link.trace_id);
        let attrs_dd = link.attributes.defensive_dedup();
        let attr_count = attrs_dd
            .iter()
            .filter(|(_, v)| matches!(v, AttributeValue::String(_) | AttributeValue::Bool(_)))
            .count() as u32;

        let link_len = 3 // trace_id, trace_id_high, span_id (always)
            + (attr_count > 0) as u32
            + (!link.tracestate.borrow().is_empty()) as u32
            + (link.flags != 0) as u32;

        write_map_len(writer, link_len)?;

        write_const_msgpack_str!(writer, "trace_id")?;
        write_u64(writer, trace_id_low)?;

        write_const_msgpack_str!(writer, "trace_id_high")?;
        write_u64(writer, trace_id_high)?;

        write_const_msgpack_str!(writer, "span_id")?;
        write_u64(writer, link.span_id)?;

        if attr_count > 0 {
            write_const_msgpack_str!(writer, "attributes")?;
            write_map_len(writer, attr_count)?;
            for (k, v) in attrs_dd.iter() {
                match v {
                    AttributeValue::String(s) => {
                        write_str(writer, k.borrow())?;
                        write_str(writer, s.borrow())?;
                    }
                    AttributeValue::Bool(b) => {
                        write_str(writer, k.borrow())?;
                        write_bool_as_str(writer, *b)?;
                    }
                    _ => {}
                }
            }
        }

        if !link.tracestate.borrow().is_empty() {
            write_const_msgpack_str!(writer, "tracestate")?;
            write_str(writer, link.tracestate.borrow())?;
        }

        if link.flags != 0 {
            write_const_msgpack_str!(writer, "flags")?;
            write_u32(writer, link.flags)?;
        }
    }

    Ok(())
}

/// Encodes [`v1::SpanEvent`](crate::span::v1::SpanEvent)s into the v0.4 msgpack wire format
/// (downgrade: v1 input → v0.4 output). Typed attributes are downgraded to the v0.4
/// `{"type": <u8>, "<kind>_value": ...}` shape — see `write_event_attr_value`. `Bytes` and
/// `KeyValue` have no v0.4 event-attribute equivalent and are dropped.
fn encode_span_events<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_events: &[SpanEvent<T>],
) -> Result<(), ValueWriteError<W::Error>> {
    write_const_msgpack_str!(writer, "span_events")?;
    write_array_len(writer, span_events.len() as u32)?;

    for event in span_events {
        let attrs_dd = event.attributes.defensive_dedup();
        let attr_count = attrs_dd
            .iter()
            .filter(|(_, v)| is_supported_event_attr(v))
            .count() as u32;

        let event_len = 2 // time_unix_nano, name (always)
            + (attr_count > 0) as u32;

        write_map_len(writer, event_len)?;

        write_const_msgpack_str!(writer, "time_unix_nano")?;
        write_u64(writer, event.time_unix_nano)?;

        write_const_msgpack_str!(writer, "name")?;
        write_str(writer, event.name.borrow())?;

        if attr_count > 0 {
            write_const_msgpack_str!(writer, "attributes")?;
            write_map_len(writer, attr_count)?;
            for (k, v) in attrs_dd.iter() {
                if !is_supported_event_attr(v) {
                    continue;
                }
                write_str(writer, k.borrow())?;
                write_event_attr_value(writer, v)?;
            }
        }
    }

    Ok(())
}

/// Returns `true` when `v` can be downgraded to a v0.4 event-attribute (scalar or scalar list).
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

macro_rules! write_type {
    ($writer:expr, $int_type:expr, $str_type:expr) => {{
        write_map_len($writer, 2)?;
        write_const_msgpack_str!($writer, "type")?;
        write_u8($writer, $int_type)?;
        write_str($writer, $str_type)?;
    }};
}

/// Writes a v0.4 event-attribute value as `{"type": <u8>, "..._value": ...}`. Scalars produce a
/// 2-entry map; `List` produces `{"type": 4, "array_value": {"values": [...]}}` with each
/// element written via `write_event_array_element`.
fn write_event_attr_value<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    v: &AttributeValue<T>,
) -> Result<(), ValueWriteError<W::Error>> {
    match v {
        AttributeValue::String(s) => {
            write_type!(writer, 0, "string_value");
            write_str(writer, s.borrow())?;
        }
        AttributeValue::Bool(b) => {
            write_type!(writer, 1, "bool_value");
            write_bool(writer, *b).map_err(ValueWriteError::InvalidDataWrite)?;
        }
        AttributeValue::Int(i) => {
            write_type!(writer, 2, "int_value");
            write_sint(writer, *i)?;
        }
        AttributeValue::Float(f) => {
            write_type!(writer, 3, "double_value");
            write_f64(writer, *f)?;
        }
        AttributeValue::List(arr) => {
            write_type!(writer, 4, "array_value");
            // Only scalar elements survive the downgrade; nested structural entries are
            // skipped because v0.4 array elements must themselves be scalar.
            let scalar_elems = arr.iter().filter(|e| is_scalar_array_elem(e));
            let elem_count = scalar_elems.clone().count() as u32;
            write_map_len(writer, 1)?;
            write_const_msgpack_str!(writer, "values")?;
            write_array_len(writer, elem_count)?;
            for elem in scalar_elems {
                write_event_array_element(writer, elem)?;
            }
        }
        AttributeValue::Bytes(_) | AttributeValue::KeyValue(_) => {
            // Filtered upstream by `is_supported_event_attr`; reachable only on a bug.
            debug_assert!(false, "unsupported event attribute variant reached writer");
        }
    }
    Ok(())
}

/// Returns `true` when `v` is a scalar that fits in a v0.4 `AttributeArrayValue` (no nesting).
fn is_scalar_array_elem<T: TraceData>(v: &AttributeValue<T>) -> bool {
    matches!(
        v,
        AttributeValue::String(_)
            | AttributeValue::Bool(_)
            | AttributeValue::Int(_)
            | AttributeValue::Float(_)
    )
}

/// Writes a v0.4 `AttributeArrayValue` (scalar). Same `{"type", "..._value"}` shape as
/// `write_event_attr_value`, minus the `Array` variant — v0.4 array elements are scalar only.
fn write_event_array_element<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    v: &AttributeValue<T>,
) -> Result<(), ValueWriteError<W::Error>> {
    match v {
        AttributeValue::String(s) => {
            write_map_len(writer, 2)?;
            write_const_msgpack_str!(writer, "type")?;
            write_u8(writer, 0)?;
            write_const_msgpack_str!(writer, "string_value")?;
            write_str(writer, s.borrow())?;
        }
        AttributeValue::Bool(b) => {
            write_map_len(writer, 2)?;
            write_const_msgpack_str!(writer, "type")?;
            write_u8(writer, 1)?;
            write_const_msgpack_str!(writer, "bool_value")?;
            write_bool(writer, *b).map_err(ValueWriteError::InvalidDataWrite)?;
        }
        AttributeValue::Int(i) => {
            write_map_len(writer, 2)?;
            write_const_msgpack_str!(writer, "type")?;
            write_u8(writer, 2)?;
            write_const_msgpack_str!(writer, "int_value")?;
            write_sint(writer, *i)?;
        }
        AttributeValue::Float(f) => {
            write_map_len(writer, 2)?;
            write_const_msgpack_str!(writer, "type")?;
            write_u8(writer, 3)?;
            write_const_msgpack_str!(writer, "double_value")?;
            write_f64(writer, *f)?;
        }
        _ => {
            // Filtered upstream by `is_scalar_array_elem`; reachable only on a bug.
            debug_assert!(false, "non-scalar array element reached writer");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the v1::Span → v0.4 downgrade encoder. Each test encodes a small
    //! `TracerPayload` via [`super::super::to_vec_from_v1`] and decodes the bytes with
    //! `rmpv` to assert on the resulting v0.4 shape — this implicitly checks that the output
    //! is also valid msgpack consumable by any standard v0.4 decoder (test-agent, agent, etc.).
    use crate::span::v1::{
        AttributeValue, AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes,
        TraceChunkBytes, TracerPayloadBytes,
    };
    use crate::span::vec_map::VecMap;
    use libdd_tinybytes::{Bytes, BytesString};
    use rmpv::Value;
    use thin_vec::ThinVec;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
    }

    /// Encodes `payload` and decodes back into `rmpv::Value`. The top level of v0.4 is an
    /// array of traces; this helper returns it as a `Vec<Value>` so tests can index in.
    fn encode_and_decode(payload: &TracerPayloadBytes) -> Vec<Value> {
        let bytes = super::super::to_vec_from_v1(payload);
        let value = rmpv::decode::read_value(&mut &bytes[..]).expect("decode failed");
        match value {
            Value::Array(traces) => traces,
            other => panic!("expected top-level array, got {other:?}"),
        }
    }

    /// Looks up `key` in a msgpack `Value::Map`. Returns `None` when absent so callers can
    /// distinguish "field missing" from "field empty".
    fn map_get<'a>(map: &'a Value, key: &str) -> Option<&'a Value> {
        let entries = match map {
            Value::Map(m) => m,
            other => panic!("expected map, got {other:?}"),
        };
        entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
    }

    /// Convenience: build a minimal single-chunk single-span payload with the v0.4-equivalent
    /// of the canonical "svc/op/res" example. Tests override fields as needed.
    fn minimal_payload(trace_id: [u8; 16], span: SpanBytes) -> TracerPayloadBytes {
        TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id,
                spans: vec![span],
                ..Default::default()
            }],
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

    #[test]
    fn basic_span_writes_required_v04_fields() {
        let payload = minimal_payload([0u8; 16], minimal_span());
        let traces = encode_and_decode(&payload);

        assert_eq!(traces.len(), 1);
        let trace = traces[0].as_array().expect("trace must be array");
        assert_eq!(trace.len(), 1);
        let span = &trace[0];

        assert_eq!(map_get(span, "service").unwrap().as_str(), Some("svc"));
        assert_eq!(map_get(span, "name").unwrap().as_str(), Some("op"));
        assert_eq!(map_get(span, "resource").unwrap().as_str(), Some("res"));
        assert_eq!(map_get(span, "span_id").unwrap().as_u64(), Some(1));
        assert_eq!(map_get(span, "trace_id").unwrap().as_u64(), Some(0));
        assert_eq!(map_get(span, "start").unwrap().as_i64(), Some(1_000));
        assert_eq!(map_get(span, "duration").unwrap().as_i64(), Some(500));
        // Optional fields must be absent when their underlying value is zero/empty.
        assert!(map_get(span, "parent_id").is_none());
        assert!(map_get(span, "error").is_none());
        assert!(map_get(span, "type").is_none());
        assert!(map_get(span, "meta").is_none());
        assert!(map_get(span, "metrics").is_none());
        assert!(map_get(span, "meta_struct").is_none());
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
        let payload = minimal_payload([0u8; 16], span);
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta must be present");

        assert_eq!(map_get(meta, "env").unwrap().as_str(), Some("prod"));
        assert_eq!(map_get(meta, "version").unwrap().as_str(), Some("1.2.3"));
        assert_eq!(map_get(meta, "component").unwrap().as_str(), Some("http"));
        assert_eq!(map_get(meta, "span.kind").unwrap().as_str(), Some("server"));
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
        let payload = minimal_payload([0u8; 16], span);
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");

        // The dedicated `span.env` field wins; the colliding attribute is dropped rather than
        // producing a duplicate `"env"` key on the wire.
        assert_eq!(map_get(meta, "env").unwrap().as_str(), Some("prod"));
        assert_eq!(map_get(meta, "http.method").unwrap().as_str(), Some("GET"));
    }

    #[test]
    fn span_kind_internal_is_not_emitted() {
        // Internal is the default and is implied by the absence of `meta["span.kind"]`.
        let payload = minimal_payload([0u8; 16], minimal_span());
        let traces = encode_and_decode(&payload);
        // meta is None overall since no other field forces it.
        assert!(map_get(&traces[0][0], "meta").is_none());
    }

    #[test]
    fn trace_id_128_bit_splits_into_low_field_and_high_meta() {
        // trace_id = 0x_DEADBEEF__CAFEBABE_DEADBEEF__CAFEBABE  (high | low)
        let mut tid = [0u8; 16];
        tid[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABE_u64.to_be_bytes());
        tid[8..].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_be_bytes());
        let payload = minimal_payload(tid, minimal_span());
        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];

        assert_eq!(
            map_get(span, "trace_id").unwrap().as_u64(),
            Some(0x0123_4567_89AB_CDEF)
        );
        let meta = map_get(span, "meta").expect("meta must be present (carries _dd.p.tid)");
        assert_eq!(
            map_get(meta, "_dd.p.tid").unwrap().as_str(),
            Some("deadbeefcafebabe"),
            "high 64 bits must be encoded as lower-case hex without the 0x prefix"
        );
    }

    #[test]
    fn trace_id_high_zero_omits_dd_p_tid() {
        // When the upper 64 bits are zero, `_dd.p.tid` must be absent so v0.4 consumers don't
        // see a redundant `"0x0"` entry.
        let mut tid = [0u8; 16];
        tid[8..].copy_from_slice(&42u64.to_be_bytes());
        let payload = minimal_payload(tid, minimal_span());
        let traces = encode_and_decode(&payload);
        assert!(map_get(&traces[0][0], "meta").is_none());
    }

    #[test]
    fn error_true_emits_one_false_omits_field() {
        let payload_err = minimal_payload(
            [0u8; 16],
            SpanBytes {
                error: true,
                ..minimal_span()
            },
        );
        let traces_err = encode_and_decode(&payload_err);
        assert_eq!(
            map_get(&traces_err[0][0], "error").unwrap().as_i64(),
            Some(1)
        );

        let payload_ok = minimal_payload([0u8; 16], minimal_span());
        let traces_ok = encode_and_decode(&payload_ok);
        assert!(map_get(&traces_ok[0][0], "error").is_none());
    }

    #[test]
    fn string_attribute_is_routed_to_meta() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        assert_eq!(map_get(meta, "http.method").unwrap().as_str(), Some("GET"));
    }

    #[test]
    fn bool_attribute_is_stringified_in_meta() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("retry"), AttributeValue::Bool(true));
        attrs.insert(bs("cached"), AttributeValue::Bool(false));
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        assert_eq!(map_get(meta, "retry").unwrap().as_str(), Some("true"));
        assert_eq!(map_get(meta, "cached").unwrap().as_str(), Some("false"));
    }

    #[test]
    fn float_and_int_attributes_route_to_metrics_as_f64() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("duration_ms"), AttributeValue::Float(12.5));
        attrs.insert(bs("status"), AttributeValue::Int(200));
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let metrics = map_get(&traces[0][0], "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "duration_ms").unwrap().as_f64(),
            Some(12.5)
        );
        // Int is cast to f64 in the v0.4 metrics map per the mapping table.
        assert_eq!(map_get(metrics, "status").unwrap().as_f64(), Some(200.0));
    }

    #[test]
    fn bytes_attribute_routes_to_meta_struct_as_msgpack_bin() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("blob"),
            AttributeValue::Bytes(Bytes::copy_from_slice(b"\xde\xad\xbe\xef")),
        );
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let ms = map_get(&traces[0][0], "meta_struct").expect("meta_struct present");
        assert_eq!(
            map_get(ms, "blob").and_then(|v| match v {
                Value::Binary(b) => Some(b.as_slice()),
                _ => None,
            }),
            Some(b"\xde\xad\xbe\xef".as_slice())
        );
    }

    #[test]
    fn list_attribute_is_flattened_into_dotted_meta_and_metrics_keys() {
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("ids"),
            AttributeValue::List(vec![
                AttributeValue::Int(1),
                AttributeValue::Int(2),
                AttributeValue::String(bs("three")),
            ]),
        );
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];
        assert!(map_get(span, "meta_struct").is_none());

        let metrics = map_get(span, "metrics").expect("metrics present");
        assert_eq!(map_get(metrics, "ids.0").unwrap().as_f64(), Some(1.0));
        assert_eq!(map_get(metrics, "ids.1").unwrap().as_f64(), Some(2.0));

        let meta = map_get(span, "meta").expect("meta present");
        assert_eq!(map_get(meta, "ids.2").unwrap().as_str(), Some("three"));
    }

    #[test]
    fn keyvalue_attribute_is_flattened_into_dotted_meta_and_metrics_keys() {
        let mut inner_kv: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        inner_kv.insert(bs("user_id"), AttributeValue::Int(42));
        inner_kv.insert(bs("name"), AttributeValue::String(bs("alice")));
        inner_kv.insert(bs("active"), AttributeValue::Bool(true));

        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("user"), AttributeValue::KeyValue(inner_kv));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];
        assert!(map_get(span, "meta_struct").is_none());

        let metrics = map_get(span, "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "user.user_id").unwrap().as_f64(),
            Some(42.0)
        );

        let meta = map_get(span, "meta").expect("meta present");
        assert_eq!(map_get(meta, "user.name").unwrap().as_str(), Some("alice"));
        assert_eq!(map_get(meta, "user.active").unwrap().as_str(), Some("true"));
    }

    #[test]
    fn nested_keyvalue_and_list_recurse_into_dotted_keys() {
        // Build: {"outer": KeyValue { "items": List [String "a", KeyValue {"k": Int 1}] }}
        let mut nested_kv: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        nested_kv.insert(bs("k"), AttributeValue::Int(1));

        let mut middle_kv: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        middle_kv.insert(
            bs("items"),
            AttributeValue::List(vec![
                AttributeValue::String(bs("a")),
                AttributeValue::KeyValue(nested_kv),
            ]),
        );

        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("outer"), AttributeValue::KeyValue(middle_kv));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                attributes: attrs,
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];
        assert!(map_get(span, "meta_struct").is_none());

        let meta = map_get(span, "meta").expect("meta present");
        assert_eq!(map_get(meta, "outer.items.0").unwrap().as_str(), Some("a"));

        let metrics = map_get(span, "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "outer.items.1.k").unwrap().as_f64(),
            Some(1.0)
        );
    }

    #[test]
    fn chunk_origin_priority_and_sampling_mechanism_propagate_to_span() {
        let chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                priority: Some(1),
                origin: bs("synthetics"),
                sampling_mechanism: Some(4),
                attributes: chunk_attrs,
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];

        let meta = map_get(span, "meta").expect("meta carries origin + sampling_mechanism");
        assert_eq!(
            map_get(meta, "_dd.origin").unwrap().as_str(),
            Some("synthetics")
        );
        assert_eq!(
            map_get(meta, "_dd.p.dm").unwrap().as_str(),
            Some("-4"),
            "sampling_mechanism is encoded as `-{{n}}` per the agent's convention"
        );

        let metrics = map_get(span, "metrics").expect("metrics carries sampling_priority_v1");
        assert_eq!(
            map_get(metrics, "_sampling_priority_v1").unwrap().as_f64(),
            Some(1.0)
        );
    }

    #[test]
    fn chunk_attributes_are_propagated_to_every_span_in_chunk() {
        let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        chunk_attrs.insert(bs("region"), AttributeValue::String(bs("us-east-1")));
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                attributes: chunk_attrs,
                spans: vec![
                    minimal_span(),
                    SpanBytes {
                        span_id: 2,
                        ..minimal_span()
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let trace = traces[0].as_array().expect("trace is array of spans");
        assert_eq!(trace.len(), 2);

        for span in trace {
            let meta = map_get(span, "meta").expect("each span inherits chunk attrs");
            assert_eq!(map_get(meta, "region").unwrap().as_str(), Some("us-east-1"));
        }
    }

    #[test]
    fn payload_env_and_app_version_are_used_when_span_leaves_them_unset() {
        let payload = TracerPayloadBytes {
            env: bs("prod"),
            app_version: bs("2.0.0"),
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        assert_eq!(map_get(meta, "env").unwrap().as_str(), Some("prod"));
        assert_eq!(map_get(meta, "version").unwrap().as_str(), Some("2.0.0"));
    }

    #[test]
    fn span_env_takes_precedence_over_payload_env() {
        let payload = TracerPayloadBytes {
            env: bs("prod"),
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                spans: vec![SpanBytes {
                    env: bs("staging"),
                    ..minimal_span()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        assert_eq!(map_get(meta, "env").unwrap().as_str(), Some("staging"));
    }

    #[test]
    fn payload_attributes_are_propagated_with_lowest_precedence() {
        let mut payload_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        payload_attrs.insert(bs("region"), AttributeValue::String(bs("us-east-1")));
        payload_attrs.insert(bs("shared"), AttributeValue::String(bs("payload")));

        let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        chunk_attrs.insert(bs("shared"), AttributeValue::String(bs("chunk")));

        let payload = TracerPayloadBytes {
            attributes: payload_attrs,
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                attributes: chunk_attrs,
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        assert_eq!(map_get(meta, "region").unwrap().as_str(), Some("us-east-1"));
        // Chunk value wins over the payload's same-named attribute.
        assert_eq!(map_get(meta, "shared").unwrap().as_str(), Some("chunk"));
    }

    #[test]
    fn dropped_trace_forces_user_reject_priority() {
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                dropped_trace: true,
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let metrics = map_get(&traces[0][0], "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "_sampling_priority_v1").unwrap().as_f64(),
            Some(-1.0)
        );
    }

    #[test]
    fn dropped_trace_keeps_existing_negative_priority() {
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                dropped_trace: true,
                priority: Some(-2),
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let metrics = map_get(&traces[0][0], "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "_sampling_priority_v1").unwrap().as_f64(),
            Some(-2.0)
        );
    }

    #[test]
    fn empty_payload_encodes_as_empty_top_level_array() {
        let payload = TracerPayloadBytes::default();
        let traces = encode_and_decode(&payload);
        assert!(traces.is_empty());
    }

    #[test]
    fn multiple_chunks_become_multiple_traces() {
        let payload = TracerPayloadBytes {
            chunks: vec![
                TraceChunkBytes {
                    trace_id: [0u8; 16],
                    spans: vec![minimal_span()],
                    ..Default::default()
                },
                TraceChunkBytes {
                    trace_id: [0u8; 16],
                    spans: vec![SpanBytes {
                        span_id: 99,
                        ..minimal_span()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        assert_eq!(traces.len(), 2);
        assert_eq!(map_get(&traces[0][0], "span_id").unwrap().as_u64(), Some(1));
        assert_eq!(
            map_get(&traces[1][0], "span_id").unwrap().as_u64(),
            Some(99)
        );
    }

    #[test]
    fn span_link_splits_trace_id_into_low_and_high_fields() {
        let mut link_tid = [0u8; 16];
        link_tid[..8].copy_from_slice(&0xAAAA_BBBB_CCCC_DDDD_u64.to_be_bytes());
        link_tid[8..].copy_from_slice(&0x1111_2222_3333_4444_u64.to_be_bytes());

        let mut link_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        link_attrs.insert(bs("link.name"), AttributeValue::String(bs("job-42")));
        link_attrs.insert(bs("link.retry"), AttributeValue::Bool(true));
        // Non-string/bool typed attrs must be dropped (v0.4 SpanLink is String→String only).
        link_attrs.insert(bs("link.count"), AttributeValue::Int(5));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_links: ThinVec::from_iter([SpanLinkBytes {
                    trace_id: link_tid,
                    span_id: 7,
                    attributes: link_attrs,
                    tracestate: bs("dd=t.dm:-1"),
                    flags: 3,
                }]),
                ..minimal_span()
            },
        );

        let traces = encode_and_decode(&payload);
        let links = map_get(&traces[0][0], "span_links").expect("span_links present");
        let links_arr = links.as_array().expect("span_links is array");
        assert_eq!(links_arr.len(), 1);
        let link = &links_arr[0];

        assert_eq!(
            map_get(link, "trace_id").unwrap().as_u64(),
            Some(0x1111_2222_3333_4444)
        );
        assert_eq!(
            map_get(link, "trace_id_high").unwrap().as_u64(),
            Some(0xAAAA_BBBB_CCCC_DDDD)
        );
        assert_eq!(map_get(link, "span_id").unwrap().as_u64(), Some(7));
        assert_eq!(
            map_get(link, "tracestate").unwrap().as_str(),
            Some("dd=t.dm:-1")
        );
        assert_eq!(map_get(link, "flags").unwrap().as_u64(), Some(3));

        let attrs = map_get(link, "attributes").expect("string attrs preserved");
        assert_eq!(
            map_get(attrs, "link.name").unwrap().as_str(),
            Some("job-42")
        );
        assert_eq!(map_get(attrs, "link.retry").unwrap().as_str(), Some("true"));
        // Int attr was dropped — v0.4 SpanLink schema cannot carry it.
        assert!(map_get(attrs, "link.count").is_none());
    }

    #[test]
    fn span_event_attributes_are_downgraded_to_v04_anyvalue_shape() {
        let mut event_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        event_attrs.insert(bs("kind"), AttributeValue::String(bs("exception")));
        event_attrs.insert(bs("escaped"), AttributeValue::Bool(true));
        event_attrs.insert(bs("count"), AttributeValue::Int(3));
        event_attrs.insert(bs("ratio"), AttributeValue::Float(0.75));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_events: ThinVec::from_iter([SpanEventBytes {
                    time_unix_nano: 1_700_000_000_000_000_000,
                    name: bs("oops"),
                    attributes: event_attrs,
                }]),
                ..minimal_span()
            },
        );

        let traces = encode_and_decode(&payload);
        let events = map_get(&traces[0][0], "span_events").expect("span_events present");
        let events_arr = events.as_array().expect("span_events is array");
        assert_eq!(events_arr.len(), 1);
        let event = &events_arr[0];

        assert_eq!(map_get(event, "name").unwrap().as_str(), Some("oops"));
        assert_eq!(
            map_get(event, "time_unix_nano").unwrap().as_u64(),
            Some(1_700_000_000_000_000_000)
        );

        // Each typed attribute decodes to a `{"type": <u8>, "<kind>_value": value}` map.
        let attrs = map_get(event, "attributes").expect("event attributes present");
        let kind = map_get(attrs, "kind").unwrap();
        assert_eq!(map_get(kind, "type").unwrap().as_u64(), Some(0));
        assert_eq!(
            map_get(kind, "string_value").unwrap().as_str(),
            Some("exception")
        );

        let escaped = map_get(attrs, "escaped").unwrap();
        assert_eq!(map_get(escaped, "type").unwrap().as_u64(), Some(1));
        assert_eq!(
            map_get(escaped, "bool_value").unwrap().as_bool(),
            Some(true)
        );

        let count = map_get(attrs, "count").unwrap();
        assert_eq!(map_get(count, "type").unwrap().as_u64(), Some(2));
        assert_eq!(map_get(count, "int_value").unwrap().as_i64(), Some(3));

        let ratio = map_get(attrs, "ratio").unwrap();
        assert_eq!(map_get(ratio, "type").unwrap().as_u64(), Some(3));
        assert_eq!(map_get(ratio, "double_value").unwrap().as_f64(), Some(0.75));
    }
}
