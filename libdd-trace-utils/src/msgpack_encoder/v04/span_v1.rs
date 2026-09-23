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
//! | `span_kind`                           | `meta["span.kind"]` (lowercase; `Internal` falls back to a `span.kind` string attribute) |
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
//! | Chunk `dropped_trace: true`           | Preserves the chunk's `metrics["_sampling_priority_v1"]`, defaulting to `-1` (USER_REJECT) only when no priority is set |
//!
//! An attribute sharing a name with one of the dedicated fields above (`env`, `version`,
//! `component`, `span.kind`, `_dd.p.tid`, `_dd.origin`, `_dd.p.dm`, `_sampling_priority_v1`) is
//! dropped: the dedicated field always wins, so each key is written at most once.

use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink};
use crate::span::vec_map::{DedupedVecMap, VecMap};
use crate::span::TraceData;
use rmp::encode::{
    write_bin, write_f64, write_i64, write_map_len, write_sint, write_str, write_u64, RmpWrite,
    ValueWriteError,
};
use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt::Write as _;

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

/// Chunk-level context propagated into the spans of a chunk when downgrading to v0.4. Built once
/// per chunk by the top-level encoder and passed by reference to `encode_span`. Also carries
/// payload-level fields (`payload_env`, `payload_app_version`, `payload_attributes`), which apply
/// as a fallback when the span itself doesn't set the equivalent field — v0.4 has neither a chunk
/// nor a payload concept, so both levels collapse onto the spans.
///
/// Generic chunk/payload attributes (and env/version fallbacks) collapse onto every span, but the
/// trace-level tags `trace_id` high half (`_dd.p.tid`), `origin` (`_dd.origin`),
/// `sampling_mechanism` (`_dd.p.dm`) and `priority` (`_sampling_priority_v1`) are emitted only on
/// the local-root span — see `encode_span`'s `is_root` argument.
///
/// `chunk_attrs_dd` / `payload_attrs_dd` are deduped once here rather than per span: unlike the
/// span's own attributes, they're identical for every span in the chunk.
pub(super) struct ChunkContext<'a, T: TraceData> {
    pub trace_id: &'a [u8; 16],
    pub priority: Option<i32>,
    pub origin: &'a T::Text,
    pub sampling_mechanism: Option<u32>,
    pub payload_env: &'a T::Text,
    pub payload_app_version: &'a T::Text,
    pub chunk_attrs_dd: DedupedVecMap<'a, T::Text, AttributeValue<T>>,
    pub payload_attrs_dd: DedupedVecMap<'a, T::Text, AttributeValue<T>>,
}

impl<'a, T: TraceData> ChunkContext<'a, T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trace_id: &'a [u8; 16],
        priority: Option<i32>,
        origin: &'a T::Text,
        sampling_mechanism: Option<u32>,
        attributes: &'a VecMap<T::Text, AttributeValue<T>>,
        payload_env: &'a T::Text,
        payload_app_version: &'a T::Text,
        payload_attributes: &'a VecMap<T::Text, AttributeValue<T>>,
    ) -> Self {
        Self {
            trace_id,
            priority,
            origin,
            sampling_mechanism,
            payload_env,
            payload_app_version,
            chunk_attrs_dd: attributes.defensive_dedup(),
            payload_attrs_dd: payload_attributes.defensive_dedup(),
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

/// Index of the chunk's local root: the first span whose parent isn't in the chunk (none, or
/// remote), else `0`. Trace-level context (`_dd.p.tid`, `_dd.origin`, ...) belongs on it only.
pub fn local_root_idx<'a, T: TraceData + 'a>(
    mut spans: impl Iterator<Item = &'a Span<T>> + Clone,
) -> usize {
    let ids: HashSet<u64> = spans.clone().map(|s| s.span_id).collect();
    spans
        .position(|s| s.parent_id == 0 || !ids.contains(&s.parent_id))
        .unwrap_or(0)
}

/// Per-bucket counts for the v0.4 `meta`, `metrics`, and `meta_struct` maps.
#[derive(Default)]
struct BucketCounts {
    meta: u32,
    metrics: u32,
    meta_struct: u32,
}

/// Drops entries whose key was already seen, keeping the first occurrence. Two distinct
/// attributes can flatten to the same dotted key (e.g. a literal `"a.0"` attribute alongside a
/// `List` attribute named `"a"`, whose first element also flattens to `"a.0"`); msgpack doesn't
/// forbid duplicate map keys, but doesn't define decoder precedence for them either, so we
/// enforce "each key written at most once" ourselves rather than leaving it to chance.
fn dedup_first_wins<V>(mut leaves: Vec<(String, V)>) -> Vec<(String, V)> {
    // Same two-pass technique as `VecMap::dedup`: collect a keep/drop bitmap using borrowed
    // `&str`s (no per-key clone), then `retain` in place (no full re-collect into a new `Vec`).
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

/// Recursively flattens a `List`/`KeyValue` attribute into dotted-key leaf entries for the v0.4
/// `meta` (string-valued) and `metrics` (numeric) maps — matching how intake/the UI expect nested
/// V1 attributes to be exploded: list elements become `key.0`, `key.1`, ... and `KeyValue`
/// members become `key.<member>` (recursively, for nested `KeyValue`/`List` values). Scalars
/// (`String`/`Bool`/`Int`/`Float`) are leaves in their own right and produce a single entry under
/// `key`. `Bytes` has no flattened form; callers must route it to `meta_struct` separately.
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
            // Reuse `key`'s buffer across siblings instead of allocating a new `String` per
            // recursion level: append the suffix, recurse, then truncate back before the next
            // sibling. Only leaves actually need an owned `String` (via `.clone()` above).
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
///
/// `is_root` marks the chunk's local-root span. Trace-level context (`_dd.p.tid`, `_dd.origin`,
/// `_dd.p.dm`, `_sampling_priority_v1`) is emitted only for it — in v0.4 those tags live on the
/// local root, never on child spans. Generic chunk/payload attributes still propagate to every
/// span (v0.4 has no chunk concept, so they collapse onto each span).
pub(super) fn encode_span<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span: &Span<T>,
    chunk: &ChunkContext<'_, T>,
    is_root: bool,
) -> Result<(), ValueWriteError<W::Error>> {
    let span_attrs_dd = span.attributes.defensive_dedup();

    // Merge span + chunk + payload attributes upfront with explicit "span overrides chunk
    // overrides payload" precedence. We don't rely on msgpack map last-write-wins decoding
    // here: the v0.4 / msgpack specs do not formalize behavior for duplicate map keys, so we
    // emit each key exactly once.
    //
    // Attributes sharing a name with a "promoted" dedicated field (`env`, `_dd.origin`, ...)
    // are dropped here: the dedicated field always wins so we never emit that key twice.
    //
    // This is a hot path (once per span), so precedence is resolved by chaining filtered
    // iterators over the already-deduped maps rather than materializing intermediate
    // `Vec`/`HashSet`s of keys — `chunk_attrs_dd`/`payload_attrs_dd` are small enough in
    // practice that the linear "is this key already present" scans below are cheaper than an
    // allocation.
    let merged_attrs = span_attrs_dd
        .iter()
        .filter(|(k, _)| !PROMOTED_ATTR_KEYS.contains(&(*k).borrow()))
        .chain(chunk.chunk_attrs_dd.iter().filter(|(k, _)| {
            !PROMOTED_ATTR_KEYS.contains(&(*k).borrow())
                && !span_attrs_dd.iter().any(|(k2, _)| k2 == *k)
        }))
        .chain(chunk.payload_attrs_dd.iter().filter(|(k, _)| {
            !PROMOTED_ATTR_KEYS.contains(&(*k).borrow())
                && !span_attrs_dd.iter().any(|(k2, _)| k2 == *k)
                && !chunk.chunk_attrs_dd.iter().any(|(k2, _)| k2 == *k)
        }));

    let (trace_id_low, trace_id_high) = split_trace_id(chunk.trace_id);
    // `Internal` is also where a non-standard kind string (e.g. "process") ends up after
    // ingestion collapses it; prefer the companion `span.kind` attribute it left behind.
    let kind_meta = span_kind_to_meta(span.span_kind).or_else(|| {
        span_attrs_dd.iter().find_map(|(k, v)| {
            if k.borrow() != "span.kind" {
                return None;
            }
            match v {
                AttributeValue::String(s) => Some(s.borrow()),
                _ => None,
            }
        })
    });

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
    let mut key_buf = String::new();
    for (k, v) in merged_attrs {
        match v {
            AttributeValue::Bytes(b) => bytes_attrs.push((k, b)),
            _ => {
                key_buf.clear();
                key_buf.push_str(k.borrow());
                flatten_attr_into(&mut key_buf, v, &mut meta_leaves, &mut metrics_leaves);
            }
        }
    }
    // Two distinct attributes can flatten to the same dotted key (see `dedup_first_wins`);
    // `meta` and `metrics` are separate wire maps, so dedup independently within each bucket.
    let meta_leaves = dedup_first_wins(meta_leaves);
    let metrics_leaves = dedup_first_wins(metrics_leaves);

    // v0.4 downgrade carries span links as the LEGACY `_dd.span_links` meta tag (a json_encode
    // string), NOT a native top-level `span_links` field — old v0.4 agents only understand the
    // legacy meta, and emitting both would double-count links on newer agents. The native
    // `span_links` field stays on the v1 wire (a different encoder); only this downgrade is legacy.
    let span_links_json =
        (!span.span_links.is_empty()).then(|| span_links_to_legacy_json(&span.span_links));
    // Same legacy-on-v0.4 treatment for span events: the LEGACY `events` meta tag (json_encode
    // string), NOT a native top-level `span_events` field, since old v0.4 agents only understand
    // the legacy meta. The native `span_events` field stays on the v1 wire (a different encoder).
    let span_events_json =
        (!span.span_events.is_empty()).then(|| span_events_to_legacy_json(&span.span_events));

    // First pass: count bucket sizes so each msgpack map header carries the exact length.
    let mut counts = BucketCounts::default();
    counts.meta += !env.is_empty() as u32;
    counts.meta += !version.is_empty() as u32;
    counts.meta += !span.component.borrow().is_empty() as u32;
    counts.meta += kind_meta.is_some() as u32;
    counts.meta += (is_root && trace_id_high != 0) as u32;
    counts.meta += (is_root && !chunk.origin.borrow().is_empty()) as u32;
    counts.meta += (is_root && chunk.sampling_mechanism.is_some()) as u32;
    counts.meta += span_links_json.is_some() as u32;
    counts.meta += span_events_json.is_some() as u32;
    counts.meta += meta_leaves.len() as u32;
    counts.metrics += (is_root && chunk.priority.is_some()) as u32;
    counts.metrics += metrics_leaves.len() as u32;
    counts.meta_struct += bytes_attrs.len() as u32;

    let span_len = 7 // service, name, resource, trace_id, span_id, start, duration (always)
        + (!span.r#type.borrow().is_empty()) as u32
        + (span.parent_id != 0) as u32
        + span.error as u32
        + (counts.meta > 0) as u32
        + (counts.metrics > 0) as u32
        + (counts.meta_struct > 0) as u32;

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
        if is_root && trace_id_high != 0 {
            // Lower-case hex without `0x` prefix — the agent expects this format.
            write_const_msgpack_str!(writer, "_dd.p.tid")?;
            let mut buf = [0u8; 16];
            let hex_str = hex::encode_to_slice(trace_id_high.to_be_bytes(), &mut buf)
                .ok()
                .and_then(|_| std::str::from_utf8(&buf).ok())
                .unwrap_or_default();
            write_str(writer, hex_str)?;
        }
        if is_root && !chunk.origin.borrow().is_empty() {
            write_const_msgpack_str!(writer, "_dd.origin")?;
            write_str(writer, chunk.origin.borrow())?;
        }
        if let Some(mechanism) = chunk.sampling_mechanism.filter(|_| is_root) {
            write_const_msgpack_str!(writer, "_dd.p.dm")?;
            // Always emit a leading '-' so mechanism 0 serializes as "-0", not "0".
            write_str(writer, &format!("-{mechanism}"))?;
        }
        if let Some(links_json) = &span_links_json {
            write_const_msgpack_str!(writer, "_dd.span_links")?;
            write_str(writer, links_json)?;
        }
        if let Some(events_json) = &span_events_json {
            write_const_msgpack_str!(writer, "events")?;
            write_str(writer, events_json)?;
        }
        for (k, v) in &meta_leaves {
            write_str(writer, k)?;
            write_str(writer, v)?;
        }
    }

    if counts.metrics > 0 {
        write_const_msgpack_str!(writer, "metrics")?;
        write_map_len(writer, counts.metrics)?;

        if let Some(priority) = chunk.priority.filter(|_| is_root) {
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

    // Span links and span events are emitted as the legacy `_dd.span_links` / `events` meta tags
    // above (v0.4 downgrade), not native top-level fields — old v0.4 agents only parse the legacy
    // meta. The native fields stay on the v1 wire (a different encoder).

    Ok(())
}

/// Serializes native v1 span links to the LEGACY v0.4 `_dd.span_links` meta value: a
/// `json_encode`-byte-identical JSON array of `{trace_id, span_id, trace_state?, attributes?}`
/// objects, exactly as master's `DDTrace\SpanLink::jsonSerialize` produced. On the v0.4 downgrade
/// (agent speaks only v0.4) old agents understand this legacy meta tag; the native top-level
/// `span_links` field is not emitted.
///
/// Format (matching master + the blessed web snapshots, byte-for-byte):
/// * `trace_id` — the full 128-bit id as 32 lowercase hex chars (high half zero-padded).
/// * `span_id` — 16 lowercase hex chars.
/// * `trace_state` — emitted only when set (master leaves the property unset → `json_encode` omits
///   it).
/// * `attributes` — a `String → String` map, emitted only when non-empty. Scalars stringify (`Bool`
///   → `"true"`/`"false"`); nested `List`/`KeyValue`/`Bytes` carry their `json_encode` string (the
///   exact bytes the pre-native tracer produced); raw `Int`/`Float` have no string slot and are
///   dropped (the tracer never emits them for links).
/// * `dropped_attributes_count` / `flags` — not part of the legacy shape (master leaves the count
///   property unset; flags is a native-only concept).
fn span_links_to_legacy_json<T: TraceData>(span_links: &[SpanLink<T>]) -> String {
    let mut out = String::from("[");
    for (i, link) in span_links.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"trace_id\":");
        json_escape_str(&mut out, &hex::encode(link.trace_id));
        out.push_str(",\"span_id\":");
        json_escape_str(&mut out, &format!("{:016x}", link.span_id));

        if !link.tracestate.borrow().is_empty() {
            out.push_str(",\"trace_state\":");
            json_escape_str(&mut out, link.tracestate.borrow());
        }

        // Iterate the link attributes in their original (PHP insertion) order — `_dd.span_links` is
        // a literal JSON string, so the attribute order must match master byte-for-byte. Link
        // attributes come from a PHP array (unique keys), so no dedup is needed.
        // v0.4 link attributes are String → String; Int/Float have no string slot and are dropped.
        let has_str_attrs = link
            .attributes
            .iter()
            .any(|(_, v)| !matches!(v, AttributeValue::Int(_) | AttributeValue::Float(_)));
        if has_str_attrs {
            out.push_str(",\"attributes\":{");
            let mut first = true;
            for (k, v) in link.attributes.iter() {
                let sval = match v {
                    AttributeValue::String(s) => s.borrow().to_owned(),
                    AttributeValue::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
                    AttributeValue::List(_)
                    | AttributeValue::KeyValue(_)
                    | AttributeValue::Bytes(_) => attr_to_php_json(v),
                    AttributeValue::Int(_) | AttributeValue::Float(_) => continue,
                };
                if !first {
                    out.push(',');
                }
                first = false;
                json_escape_str(&mut out, k.borrow());
                out.push(':');
                json_escape_str(&mut out, &sval);
            }
            out.push('}');
        }

        out.push('}');
    }
    out.push(']');
    out
}

/// Serializes native v1 span events to the LEGACY v0.4 `events` meta value: a
/// `json_encode`-byte-identical JSON array of `{name, time_unix_nano, attributes?}` objects,
/// exactly as master's `DDTrace\SpanEvent::jsonSerialize` produced. On the v0.4 downgrade (agent
/// speaks only v0.4) old agents understand this legacy meta tag; the native top-level `span_events`
/// field is not emitted.
///
/// Format (matching master + master's `dd_trace_span_event.phpt`, byte-for-byte):
/// * `name` — the event name (json string).
/// * `time_unix_nano` — the timestamp as an unquoted JSON number.
/// * `attributes` — emitted only when non-empty. Unlike links (which are `String → String`), event
///   attributes keep their NATIVE JSON types: `Int`/`Float` → numbers, `Bool` → `true`/`false`,
///   nested `List`/`KeyValue` → real JSON arrays/objects (recursively typed) — the exact
///   `json_encode` of the PHP attributes array. Values are produced by [`attr_to_php_json`].
fn span_events_to_legacy_json<T: TraceData>(span_events: &[SpanEvent<T>]) -> String {
    let mut out = String::from("[");
    for (i, event) in span_events.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"name\":");
        json_escape_str(&mut out, event.name.borrow());
        out.push_str(",\"time_unix_nano\":");
        let _ = write!(out, "{}", event.time_unix_nano);

        // Iterate attributes in their original (PHP insertion) order — `events` is a literal JSON
        // string, so order must match master byte-for-byte. Event attributes come from a PHP array
        // (unique keys), so no dedup is needed. All types are kept (json_encode types faithfully).
        if !event.attributes.is_empty() {
            out.push_str(",\"attributes\":{");
            for (j, (k, v)) in event.attributes.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                json_escape_str(&mut out, k.borrow());
                out.push(':');
                write_attr_json(&mut out, v);
            }
            out.push('}');
        }

        out.push('}');
    }
    out.push(']');
    out
}

/// Serializes a non-scalar link/event `AttributeValue` (`List`/`KeyValue`/`Bytes`) into a JSON
/// string byte-identical to PHP's `json_encode($value)` with default flags — the exact bytes the
/// tracer's C serializer produced for these attributes before native nested attributes existed.
///
/// v0.4 link/event attributes have no nested representation, so the pre-native wire always carried
/// the `json_encode` string; reproducing it here keeps that wire unchanged for old agents. Floats
/// follow PHP's `serialize_precision=-1` format (see [`write_php_json_float`]). `Bytes` cannot
/// originate from a PHP value and is encoded defensively as a (lossy-UTF-8) JSON string.
fn attr_to_php_json<T: TraceData>(v: &AttributeValue<T>) -> String {
    let mut out = String::new();
    write_attr_json(&mut out, v);
    out
}

fn write_attr_json<T: TraceData>(out: &mut String, v: &AttributeValue<T>) {
    match v {
        AttributeValue::String(s) => json_escape_str(out, s.borrow()),
        AttributeValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        AttributeValue::Int(i) => {
            let _ = write!(out, "{i}");
        }
        AttributeValue::Float(f) => write_php_json_float(out, *f),
        AttributeValue::Bytes(b) => json_escape_str(out, &String::from_utf8_lossy(b.borrow())),
        AttributeValue::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_attr_json(out, item);
            }
            out.push(']');
        }
        AttributeValue::KeyValue(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_escape_str(out, k.borrow());
                out.push(':');
                write_attr_json(out, val);
            }
            out.push('}');
        }
    }
}

/// Appends `f` exactly as PHP's `json_encode` prints it (`zend_gcvt` mode 0): shortest round-trip
/// digits, in exponent form (`1.0e+20`, `1.0e-5`) when the decimal point position is > 17 or < -3.
fn write_php_json_float(out: &mut String, f: f64) {
    if !f.is_finite() {
        // json_encode rejects Inf/NaN; 0 is what it substitutes with JSON_PARTIAL_OUTPUT_ON_ERROR.
        out.push('0');
        return;
    }
    if f.is_sign_negative() {
        out.push('-');
    }
    let (digits, exp) = php_shortest_digits(f.abs());
    let decpt = exp + 1; // zend_dtoa convention: value = 0.DIGITS * 10^decpt
    if !(-3..=17).contains(&decpt) {
        out.push_str(&digits[..1]);
        out.push('.');
        out.push_str(if digits.len() > 1 { &digits[1..] } else { "0" });
        let sign = if exp < 0 { '-' } else { '+' };
        let _ = write!(out, "e{sign}{}", exp.unsigned_abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat(decpt.unsigned_abs() as usize));
        out.push_str(&digits);
    } else {
        let int_len = decpt as usize;
        if digits.len() <= int_len {
            out.push_str(&digits);
            out.push_str(&"0".repeat(int_len - digits.len()));
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    }
}

/// Shortest round-trip significant digits and scientific exponent of a finite `f >= 0`. Rust rounds
/// exact halfway ties up where zend_dtoa rounds them to even (e.g. 110767565253548.125 -> ...12).
fn php_shortest_digits(f: f64) -> (String, i32) {
    let split = |s: &str| -> (String, i32) {
        let (m, e) = s.split_once('e').unwrap_or((s, "0"));
        (m.replace('.', ""), e.parse().unwrap_or(0))
    };
    let (digits, exp) = split(&format!("{f:e}"));
    let last = digits.as_bytes()[digits.len() - 1] - b'0';
    if last % 2 == 1 {
        let head = &digits[..digits.len() - 1];
        for alt in [last - 1, last + 1].into_iter().filter(|&d| d <= 9) {
            let cand = format!("{head}{alt}");
            if format!("{}.{}e{exp}", &cand[..1], &cand[1..]).parse::<f64>() != Ok(f) {
                continue;
            }
            // A tie iff f's exact decimal expansion (<= 767 digits) is "<lower candidate>5000...".
            let (exact, exact_exp) = split(&format!("{f:.800e}"));
            let lower = if alt < last { &cand } else { &digits };
            if exact_exp == exp {
                if let Some(rest) = exact.strip_prefix(lower.as_str()) {
                    if rest.starts_with('5') && rest[1..].bytes().all(|b| b == b'0') {
                        return (cand, exp);
                    }
                }
            }
        }
    }
    (digits, exp)
}

/// Appends `s` as a JSON string literal (surrounding quotes included), escaped exactly like PHP's
/// `json_encode` with default flags: `"`, `\`, `/`, the `\b \f \n \r \t` shorthands, other control
/// chars and every non-ASCII code point as a lowercase `\uXXXX` escape (UTF-16, surrogate pairs
/// above U+FFFF). The result is therefore pure ASCII.
fn json_escape_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let cp = c as u32;
                if cp <= 0xFFFF {
                    let _ = write!(out, "\\u{cp:04x}");
                } else {
                    let v = cp - 0x10000;
                    let hi = 0xD800 + (v >> 10);
                    let lo = 0xDC00 + (v & 0x3FF);
                    let _ = write!(out, "\\u{hi:04x}\\u{lo:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    //! Unit tests for the v1::Span → v0.4 downgrade encoder. Each test encodes a small
    //! `TracerPayload` via [`super::super::to_vec_from_v1`] and decodes the bytes with
    //! `rmpv` to assert on the resulting v0.4 shape — this implicitly checks that the output
    //! is also valid msgpack consumable by any standard v0.4 decoder (test-agent, agent, etc.).
    use super::attr_to_php_json;
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
    fn tid_attribute_is_never_emitted_from_the_attribute_map() {
        // A user-set `_dd.p.tid` attribute is dropped on every span: the local root carries the
        // chunk trace-id high half instead, other spans carry nothing.
        let tid_attr = || {
            let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
            attrs.insert(bs("_dd.p.tid"), AttributeValue::String(bs("user-set")));
            attrs
        };
        let root = SpanBytes {
            attributes: tid_attr(),
            ..minimal_span()
        };
        let child = SpanBytes {
            span_id: 2,
            parent_id: 1,
            attributes: tid_attr(),
            ..minimal_span()
        };
        let mut trace_id = [0u8; 16];
        trace_id[..8].copy_from_slice(&0xdead_beef_u64.to_be_bytes());
        let mut payload = minimal_payload(trace_id, root);
        payload.chunks[0].spans.push(child);
        let traces = encode_and_decode(&payload);

        let root_meta = map_get(&traces[0][0], "meta").expect("root meta present");
        assert_eq!(
            map_get(root_meta, "_dd.p.tid").unwrap().as_str(),
            Some("00000000deadbeef")
        );
        assert!(map_get(&traces[0][1], "meta").is_none_or(|m| map_get(m, "_dd.p.tid").is_none()));
    }

    #[test]
    fn flattened_attribute_colliding_with_another_attribute_keeps_first_wins() {
        // "a" is a List whose first element flattens to "a.0"; "a.0" is also a literal
        // attribute key. Both flatten to the same dotted key "a.0" — only one must survive
        // on the wire, keeping whichever attribute came first in insertion order.
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(
            bs("a"),
            AttributeValue::List(vec![AttributeValue::String(bs("from-list"))]),
        );
        attrs.insert(bs("a.0"), AttributeValue::String(bs("from-literal")));
        // Mark deduped (as attributes are expected to be by the time they reach the encoder)
        // so iteration order is deterministic rather than depending on `defensive_dedup`'s
        // fallback `HashMap` order.
        attrs.dedup();
        let span = SpanBytes {
            attributes: attrs,
            ..minimal_span()
        };
        let payload = minimal_payload([0u8; 16], span);
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        let meta_entries = meta.as_map().expect("meta must be a map");

        // Only one "a.0" entry must survive on the wire, regardless of which attribute wins.
        let a0_count = meta_entries
            .iter()
            .filter(|(k, _)| k.as_str() == Some("a.0"))
            .count();
        assert_eq!(a0_count, 1, "duplicate \"a.0\" key written to the wire");
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
    fn span_kind_internal_with_preserved_string_attribute_round_trips() {
        // A non-standard kind (e.g. "process") collapses to Internal at ingestion but survives
        // as a companion `span.kind` attribute; the downgrade must prefer it over omitting the key.
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("span.kind"), AttributeValue::String(bs("process")));
        let span = SpanBytes {
            span_kind: SpanKind::Internal,
            attributes: attrs,
            ..minimal_span()
        };
        let payload = minimal_payload([0u8; 16], span);
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta must be present");

        assert_eq!(
            map_get(meta, "span.kind").unwrap().as_str(),
            Some("process")
        );
        // Must be written exactly once (promoted, not duplicated as a generic attribute too).
        let meta_entries = meta.as_map().expect("meta must be a map");
        let kind_count = meta_entries
            .iter()
            .filter(|(k, _)| k.as_str() == Some("span.kind"))
            .count();
        assert_eq!(
            kind_count, 1,
            "duplicate \"span.kind\" key written to the wire"
        );
    }

    #[test]
    fn span_kind_known_value_ignores_stray_span_kind_attribute() {
        // A recognized kind always wins over any (unexpected) `span.kind` attribute — the
        // attribute fallback only kicks in for `Internal`.
        let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        attrs.insert(bs("span.kind"), AttributeValue::String(bs("stale")));
        let span = SpanBytes {
            span_kind: SpanKind::Server,
            attributes: attrs,
            ..minimal_span()
        };
        let payload = minimal_payload([0u8; 16], span);
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta must be present");

        assert_eq!(map_get(meta, "span.kind").unwrap().as_str(), Some("server"));
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
    fn trace_level_tags_only_on_local_root_not_children() {
        // _dd.p.tid / _dd.origin / _dd.p.dm / _sampling_priority_v1 are trace-level in v0.4 and
        // belong ONLY on the local-root span (parent_id == 0 here). A multi-span chunk must not
        // stamp them onto child spans — doing so is what RC-A ("trace tags leak onto children")
        // was: the downgrade injected chunk-level context into every span unconditionally.
        let mut trace_id = [0u8; 16];
        trace_id[7] = 0xAB; // non-zero high half -> _dd.p.tid
        let root = SpanBytes {
            span_id: 1,
            parent_id: 0,
            ..minimal_span()
        };
        let child = SpanBytes {
            span_id: 2,
            parent_id: 1,
            ..minimal_span()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id,
                priority: Some(1),
                origin: bs("synthetics"),
                sampling_mechanism: Some(4),
                spans: vec![root, child],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let trace = traces[0].as_array().expect("trace is array");
        assert_eq!(trace.len(), 2);

        // Root (index 0) carries every trace-level tag.
        let root_meta = map_get(&trace[0], "meta").expect("root has meta");
        assert!(
            map_get(root_meta, "_dd.p.tid").is_some(),
            "root must carry _dd.p.tid"
        );
        assert_eq!(
            map_get(root_meta, "_dd.origin").unwrap().as_str(),
            Some("synthetics")
        );
        assert_eq!(map_get(root_meta, "_dd.p.dm").unwrap().as_str(), Some("-4"));
        let root_metrics = map_get(&trace[0], "metrics").expect("root has metrics");
        assert_eq!(
            map_get(root_metrics, "_sampling_priority_v1")
                .unwrap()
                .as_f64(),
            Some(1.0)
        );

        // Child (index 1) must NOT carry any trace-level tag.
        if let Some(child_meta) = map_get(&trace[1], "meta") {
            assert!(
                map_get(child_meta, "_dd.p.tid").is_none(),
                "_dd.p.tid leaked onto child"
            );
            assert!(
                map_get(child_meta, "_dd.origin").is_none(),
                "_dd.origin leaked onto child"
            );
            assert!(
                map_get(child_meta, "_dd.p.dm").is_none(),
                "_dd.p.dm leaked onto child"
            );
        }
        if let Some(child_metrics) = map_get(&trace[1], "metrics") {
            assert!(
                map_get(child_metrics, "_sampling_priority_v1").is_none(),
                "_sampling_priority_v1 leaked onto child"
            );
        }
    }

    #[test]
    fn trace_level_tags_land_on_top_level_span_with_remote_parent() {
        // Distributed trace: the local root has a non-zero parent_id (remote parent) but is
        // marked _dd.top_level=1. The trace-level tags must land on it, not the first-listed span.
        let mut top_level_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        top_level_attrs.insert(bs("_dd.top_level"), AttributeValue::Float(1.0));
        let leaf = SpanBytes {
            span_id: 5,
            parent_id: 9, // remote parent, not top level
            ..minimal_span()
        };
        let local_root = SpanBytes {
            span_id: 9,
            parent_id: 100, // remote parent
            attributes: top_level_attrs,
            ..minimal_span()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                origin: bs("rum"),
                sampling_mechanism: Some(3),
                spans: vec![leaf, local_root],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let trace = traces[0].as_array().expect("trace is array");

        // trace[0] is the leaf (non-root) — no trace-level tags.
        if let Some(leaf_meta) = map_get(&trace[0], "meta") {
            assert!(
                map_get(leaf_meta, "_dd.origin").is_none(),
                "_dd.origin leaked onto non-root"
            );
            assert!(
                map_get(leaf_meta, "_dd.p.dm").is_none(),
                "_dd.p.dm leaked onto non-root"
            );
        }
        // trace[1] is the _dd.top_level local root — it carries them.
        let root_meta = map_get(&trace[1], "meta").expect("local root has meta");
        assert_eq!(
            map_get(root_meta, "_dd.origin").unwrap().as_str(),
            Some("rum")
        );
        assert_eq!(map_get(root_meta, "_dd.p.dm").unwrap().as_str(), Some("-3"));
    }

    #[test]
    fn trace_level_tags_land_on_remote_parent_root_not_first_span() {
        // Inferred proxy: the web span is listed first but its parent (the inferred span, whose
        // own parent is remote) is in the chunk, so the inferred span is the local root.
        let web = SpanBytes {
            span_id: 7,
            parent_id: 9,
            ..minimal_span()
        };
        let inferred = SpanBytes {
            span_id: 9,
            parent_id: 2,
            ..minimal_span()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                origin: bs("rum"),
                spans: vec![web, inferred],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let trace = traces[0].as_array().expect("trace is array");

        if let Some(web_meta) = map_get(&trace[0], "meta") {
            assert!(
                map_get(web_meta, "_dd.origin").is_none(),
                "_dd.origin leaked onto non-root"
            );
        }
        let root_meta = map_get(&trace[1], "meta").expect("local root has meta");
        assert_eq!(
            map_get(root_meta, "_dd.origin").unwrap().as_str(),
            Some("rum")
        );
    }

    #[test]
    fn sampling_mechanism_zero_encodes_as_negative_zero() {
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                sampling_mechanism: Some(0),
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta carries sampling_mechanism");
        assert_eq!(
            map_get(meta, "_dd.p.dm").unwrap().as_str(),
            Some("-0"),
            "mechanism 0 must serialize as `-0`, not `0`, per the agent's convention"
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
    fn dropped_trace_preserves_priority() {
        // A dropped_trace chunk keeps its own priority: AUTO_REJECT `0` stays `0`.
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                dropped_trace: true,
                priority: Some(0),
                spans: vec![minimal_span()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let traces = encode_and_decode(&payload);
        let metrics = map_get(&traces[0][0], "metrics").expect("metrics present");
        assert_eq!(
            map_get(metrics, "_sampling_priority_v1").unwrap().as_f64(),
            Some(0.0)
        );
    }

    #[test]
    fn dropped_trace_without_priority_defaults_to_user_reject() {
        // With no priority set, a dropped_trace chunk defaults to `-1` (USER_REJECT).
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
        let span = &traces[0][0];
        // No native span_links field on the v0.4 downgrade — links go to `_dd.span_links` meta.
        assert!(map_get(span, "span_links").is_none());
        let meta = map_get(span, "meta").expect("meta present");
        // Full 128-bit trace_id as 32 hex, span_id as 16 hex, trace_state (underscore) present,
        // Bool stringified, Int dropped, flags/dropped_attributes_count absent (legacy shape).
        let expected = "[{\"trace_id\":\"aaaabbbbccccdddd1111222233334444\",\
\"span_id\":\"0000000000000007\",\"trace_state\":\"dd=t.dm:-1\",\
\"attributes\":{\"link.name\":\"job-42\",\"link.retry\":\"true\"}}]";
        assert_eq!(
            map_get(meta, "_dd.span_links").unwrap().as_str(),
            Some(expected)
        );
    }

    #[test]
    fn span_links_downgrade_to_legacy_dd_span_links_meta_bytematch() {
        // v0.4 downgrade (agent speaks only v0.4): native span links serialize to the LEGACY
        // `_dd.span_links` meta JSON string (byte-identical to master's SpanLink::jsonSerialize),
        // and the native top-level `span_links` field is NOT emitted. Byte target is the blessed
        // snapshot tests/snapshots/tests.integrations.kafka_test.test_span_links_*.json.
        let mut link_tid = [0u8; 16]; // high = 0, low = 0xc151df7d6ee5e2d6
        link_tid[8..].copy_from_slice(&0xc151_df7d_6ee5_e2d6_u64.to_be_bytes());
        let mut link_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        link_attrs.insert(bs("_dd.p.dm"), AttributeValue::String(bs("-0")));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_links: ThinVec::from_iter([SpanLinkBytes {
                    trace_id: link_tid,
                    span_id: 0xc151_df7d_6ee5_e2d6,
                    attributes: link_attrs,
                    tracestate: bs("dd=t.dm:-0"),
                    flags: 0,
                }]),
                ..minimal_span()
            },
        );

        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];

        // Native span_links field must be ABSENT on the v0.4 wire.
        assert!(
            map_get(span, "span_links").is_none(),
            "native span_links field must not be emitted on the v0.4 downgrade"
        );

        // Legacy `_dd.span_links` meta present and byte-identical to master's json_encode output.
        let meta = map_get(span, "meta").expect("meta present");
        let expected = "[{\"trace_id\":\"0000000000000000c151df7d6ee5e2d6\",\
\"span_id\":\"c151df7d6ee5e2d6\",\"trace_state\":\"dd=t.dm:-0\",\
\"attributes\":{\"_dd.p.dm\":\"-0\"}}]";
        assert_eq!(
            map_get(meta, "_dd.span_links").unwrap().as_str(),
            Some(expected)
        );
    }

    #[test]
    fn span_links_legacy_meta_omits_tracestate_and_attributes_when_unset() {
        // A link with no tracestate and no attributes: legacy JSON carries only trace_id/span_id
        // (master omits trace_state and attributes when their PHP properties are unset).
        let mut link_tid = [0u8; 16];
        link_tid[8..].copy_from_slice(&0x1111_2222_3333_4444_u64.to_be_bytes());
        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_links: ThinVec::from_iter([SpanLinkBytes {
                    trace_id: link_tid,
                    span_id: 7,
                    attributes: VecMap::new(),
                    tracestate: bs(""),
                    flags: 0,
                }]),
                ..minimal_span()
            },
        );
        let traces = encode_and_decode(&payload);
        let meta = map_get(&traces[0][0], "meta").expect("meta present");
        let expected =
            "[{\"trace_id\":\"00000000000000001111222233334444\",\"span_id\":\"0000000000000007\"}]";
        assert_eq!(
            map_get(meta, "_dd.span_links").unwrap().as_str(),
            Some(expected)
        );
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
        let span = &traces[0][0];
        // No native span_events field on the v0.4 downgrade — events go to the legacy `events`
        // meta, json_encode-byte-identical to master (native attribute types preserved:
        // bool/int/float stay JSON bool/number, NOT the native `{"type":..}` shape).
        assert!(map_get(span, "span_events").is_none());
        let meta = map_get(span, "meta").expect("meta present");
        let expected = "[{\"name\":\"oops\",\"time_unix_nano\":1700000000000000000,\
\"attributes\":{\"kind\":\"exception\",\"escaped\":true,\"count\":3,\"ratio\":0.75}}]";
        assert_eq!(map_get(meta, "events").unwrap().as_str(), Some(expected));
    }

    /// Locks `attr_to_php_json` to PHP `json_encode($v)` (default flags): slash + non-ASCII
    /// escaping, whole-number floats without a trailing `.0`, list vs object, nested structures.
    /// The right-hand strings are the literal bytes captured from `php -r 'echo json_encode(...)'`.
    #[test]
    fn attr_to_php_json_matches_php_json_encode() {
        let list = |v: Vec<AttributeValueBytes>| AttributeValue::List(v);
        let s = |x: &str| AttributeValue::String(bs(x));
        let mut ab: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        ab.insert(bs("a"), AttributeValue::Int(1));
        ab.insert(
            bs("b"),
            list(vec![AttributeValue::Int(2), AttributeValue::Int(3)]),
        );

        assert_eq!(
            attr_to_php_json(&list(vec![AttributeValue::Int(3), AttributeValue::Int(4)])),
            "[3,4]"
        );
        assert_eq!(
            attr_to_php_json(&list(vec![s("5"), s("6")])),
            r#"["5","6"]"#
        );
        assert_eq!(
            attr_to_php_json(&AttributeValue::KeyValue(ab)),
            r#"{"a":1,"b":[2,3]}"#
        );
        // Whole-number floats drop the fractional part; decimal-range floats round-trip shortest.
        assert_eq!(
            attr_to_php_json(&list(vec![AttributeValue::Float(1.0)])),
            "[1]"
        );
        assert_eq!(
            attr_to_php_json(&list(vec![AttributeValue::Float(0.75)])),
            "[0.75]"
        );
        assert_eq!(
            attr_to_php_json(&list(vec![AttributeValue::Float(1.5)])),
            "[1.5]"
        );
        // String escaping: forward slash, quote/backslash, control shorthands, non-ASCII, astral.
        assert_eq!(attr_to_php_json(&list(vec![s("a/b")])), r#"["a\/b"]"#);
        assert_eq!(attr_to_php_json(&list(vec![s("q\"\\")])), r#"["q\"\\"]"#);
        assert_eq!(attr_to_php_json(&list(vec![s("t\tn\n")])), r#"["t\tn\n"]"#);
        // Build expected `\uXXXX` escapes via an explicit backslash so no literal `\u` bigram
        // appears in this source (non-ASCII escapes are what json_encode's default flags emit).
        let bslash = '\\';
        assert_eq!(
            attr_to_php_json(&list(vec![s("é")])),
            format!("[\"{bslash}u00e9\"]")
        );
        assert_eq!(
            attr_to_php_json(&list(vec![s("😀")])),
            format!("[\"{bslash}ud83d{bslash}ude00\"]")
        );
        assert_eq!(
            attr_to_php_json(&list(vec![AttributeValue::Bool(true)])),
            "[true]"
        );
    }

    /// Locks float formatting to PHP `json_encode($f)` (serialize_precision=-1). The right-hand
    /// strings are the literal output of `php -r 'echo json_encode($f);'` (PHP 8.3).
    #[test]
    fn attr_to_php_json_floats_match_php_json_encode() {
        let cases: &[(f64, &str)] = &[
            (1e20, "1.0e+20"),
            (-1e20, "-1.0e+20"),
            (1e-5, "1.0e-5"),
            (9.99e-5, "9.99e-5"),
            (1e-4, "0.0001"),
            (0.1, "0.1"),
            (0.05, "0.05"),
            (1.5, "1.5"),
            (1e15, "1000000000000000"),
            (1e16, "10000000000000000"),
            (1e17, "1.0e+17"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (12345678901234567.0, "12345678901234568"),
            (1.0 / 3.0, "0.3333333333333333"),
            (-2.5e-7, "-2.5e-7"),
            (1e100, "1.0e+100"),
            (f64::MAX, "1.7976931348623157e+308"),
            (5e-324, "5.0e-324"),
            (0.0, "0"),
            (-0.0, "-0"),
            (1.0, "1"),
            (3.0, "3"),
            (100.0, "100"),
            // 110767565253548.125 exactly, a halfway tie: zend_dtoa rounds to even, Rust up.
            (f64::from_bits(0x42d9_2f85_584a_eb08), "110767565253548.12"),
        ];
        for &(f, want) in cases {
            assert_eq!(
                attr_to_php_json(&AttributeValueBytes::Float(f)),
                want,
                "{f:e}"
            );
        }
        // Inf/NaN have no JSON form; json_encode's partial-output substitute is 0.
        assert_eq!(attr_to_php_json(&AttributeValueBytes::Float(f64::NAN)), "0");
    }

    #[test]
    fn span_link_nested_attr_downgrades_to_json_string() {
        // A link attribute holding a native nested list must land on the v0.4 wire as the exact
        // JSON string the pre-native tracer emitted, not be dropped.
        let mut link_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        link_attrs.insert(bs("plain"), AttributeValue::String(bs("v")));
        link_attrs.insert(
            bs("nums"),
            AttributeValue::List(vec![AttributeValue::Int(3), AttributeValue::Int(4)]),
        );
        let mut kv: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        kv.insert(bs("a"), AttributeValue::Int(1));
        link_attrs.insert(bs("obj"), AttributeValue::KeyValue(kv));

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_links: ThinVec::from_iter([SpanLinkBytes {
                    trace_id: [0u8; 16],
                    span_id: 7,
                    attributes: link_attrs,
                    tracestate: bs(""),
                    flags: 0,
                }]),
                ..minimal_span()
            },
        );

        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];
        assert!(map_get(span, "span_links").is_none());
        let meta = map_get(span, "meta").expect("meta present");
        // Nested List/KeyValue link attrs carry their json_encode string inside the legacy
        // `_dd.span_links` attributes map (the exact bytes the pre-native tracer produced).
        let expected = "[{\"trace_id\":\"00000000000000000000000000000000\",\
\"span_id\":\"0000000000000007\",\
\"attributes\":{\"plain\":\"v\",\"nums\":\"[3,4]\",\"obj\":\"{\\\"a\\\":1}\"}}]";
        assert_eq!(
            map_get(meta, "_dd.span_links").unwrap().as_str(),
            Some(expected)
        );
    }

    #[test]
    fn span_event_nested_attr_in_legacy_events_meta_keeps_native_json_arrays() {
        // Unlike links (String → String), event attributes keep native JSON types in the legacy
        // `events` meta: a nested list is a real JSON array, not a stringified `string_value`.
        // Byte target: master's tests/ext/request-replayer/dd_trace_span_event.phpt.
        let mut event_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        event_attrs.insert(bs("arg1"), AttributeValue::String(bs("value1")));
        event_attrs.insert(
            bs("int_array"),
            AttributeValue::List(vec![AttributeValue::Int(3), AttributeValue::Int(4)]),
        );
        event_attrs.insert(
            bs("string_array"),
            AttributeValue::List(vec![
                AttributeValue::String(bs("5")),
                AttributeValue::String(bs("6")),
            ]),
        );

        let payload = minimal_payload(
            [0u8; 16],
            SpanBytes {
                span_events: ThinVec::from_iter([SpanEventBytes {
                    time_unix_nano: 1720037568765201300,
                    name: bs("event-name"),
                    attributes: event_attrs,
                }]),
                ..minimal_span()
            },
        );

        let traces = encode_and_decode(&payload);
        let span = &traces[0][0];
        assert!(map_get(span, "span_events").is_none());
        let meta = map_get(span, "meta").expect("meta present");
        let expected = "[{\"name\":\"event-name\",\"time_unix_nano\":1720037568765201300,\
\"attributes\":{\"arg1\":\"value1\",\"int_array\":[3,4],\"string_array\":[\"5\",\"6\"]}}]";
        assert_eq!(map_get(meta, "events").unwrap().as_str(), Some(expected));
    }
}
