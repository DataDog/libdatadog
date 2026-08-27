// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM JSON encoder.
//!
//! Encodes Datadog v04 trace chunks to the JSON body
//! accepted by the Datadog HTTP trace intake (`POST /v1/input`).
//!
//! ## Differences from the regular agent (msgpack v04) encoding
//!
//! - **Wire format**: JSON, wrapped as `{"traces": [ {hostname, env, ..., spans: [...] }, ... ]}`.
//!   Per-trace metadata (hostname, env, language*, tracerVersion, runtimeID, containerID) is
//!   inlined on each trace instead of being carried in request headers. Hostname is always emitted
//! - **IDs**: `trace_id`, `span_id`, `parent_id` are lowercase hex strings (16 chars; 32 for
//!   span-link trace IDs)
//! - **128-bit trace IDs**: only the low 64 bits go into `trace_id`; the `_dd.p.tid` meta tag
//!   carries upper 64 bits
//! - **Span links / events**: not top-level fields. They are JSON-stringified into
//!   `meta["_dd.span_links"]` and `meta["events"]`, each truncated to 25_000 chars. No top-level
//!   `links` field is emitted. If meta attributes for "_dd.span_links" of "events" are already
//!   attached to the span we will keep existing fields and the span level field will be dropped
//! - **Stats / top-level flags**: the intake has no trace-agent to compute them, so the encoder
//!   injects `meta["_dd.compute_stats"]="1"` on the first span of each chunk and
//!   `metrics["_trace_root"]=1` where applicable.
//! - **Non-finite metrics** (NaN/Inf) are dropped (JSON can't represent them).
//!
//! TODO: span normalization (service/name/resource/type truncation + defaults)

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
use crate::span::v1::{
    AttributeValue as AttributeValueV1, Span as SpanV1, SpanEvent as SpanEventV1, SpanKind,
    SpanLink as SpanLinkV1, TraceChunk,
};
use crate::span::{TraceData, SPAN_LINK_FLAGS_SET_SENTINEL};
use crate::tracer_metadata::TracerMetadata;
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Serializer,
};
use std::borrow::Borrow;
use std::collections::HashSet;
use std::fmt::Write as _;

/// Maximum allowed size of a `meta` value before truncation.
const MAX_META_VALUE_LEN: usize = 25_000;
/// Suffix appended when a `meta` value is truncated.
const TRUNCATION_SUFFIX: &str = "...";

/// # Why are we doing this?
///
/// The JSON agentless format is different from the in-memory model of v04 spans (and to v03/v04/v05
/// on-the-wire schemas) In order to not have to copy to intermediary structs, we have to write a
/// manual encoder. For JSON there is no widely available JSON emitter in rust other than serde
/// JSON. But serde does not let us drive serialization other than through the serde::Serialize
/// trait.
///
/// Defining structs implementing serde::Serialize for every nested object in the span is heavy,
///
/// This macro captures parameters from the environment and creates a local struct implementing
/// serde::Serialize, with a custom implementation.
///
/// # Usage
///
/// The shape of the input of the macro is made to look like a closure
/// Contrary to a closure, the names of the types have to be named in full
/// ```ignore
///        Optional generic| serializer|
///            parameter   |.  |               Captured variables from env       
///         --------------  --- ---------------------------------------------------------
/// ser_fn!(<T: TraceData> |ser, traces: &'a [Vec<Span<T>>], metadata: &'a TracerMetadata| {
///     // Body of the closure
/// }
/// ```
macro_rules! ser_fn {
    ($(<$generic:ident $(: $bound:ident )?>)? |$serializer:ident , $($captured:ident : $ty:ty),+ $(,)?| { $($body:tt)* }) => {
        {
            struct SerializeClosure<'a, $($generic $(: $bound + 'a)? ,)?>(($(&'a $ty ,)*));

            impl <'a, $($generic $(: $bound + 'a)?,)?> serde::Serialize for SerializeClosure<'a, $($generic,)?> {
                #[inline]
                fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                    let captured = self.0;
                    (|$serializer: S , ($(& $captured, )*) : ($(&'a $ty ,)*)| {
                        $($body)*
                    })(serializer, captured)
                }
            }

            SerializeClosure(($(& $captured ,)*))
        }
    }
}

/// Encode the given `traces` to the agentless JSON payload (`/v1/input` body).
///
/// Returns the serialized JSON bytes on success.
pub fn encode_payload<T: TraceData>(
    traces: &[Vec<Span<T>>],
    metadata: &TracerMetadata,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut bytes);

    let mut map_ser = serializer.serialize_map(Some(1))?;
    map_ser.serialize_entry(
        "traces",
        &ser_fn!(<T: TraceData> |ser, traces: &'a [Vec<Span<T>>], metadata: &'a TracerMetadata| {
            let mut traces_serializer = ser.serialize_seq(Some(traces.len()))?;
            for chunk in traces {
                traces_serializer.serialize_element(&ser_fn!(<T: TraceData> |ser, chunk: &'a Vec<Span<T>>, metadata: &'a TracerMetadata| {
                    encode_trace(ser, chunk, metadata)
                }))?;
            }
            traces_serializer.end()
        }),
    )?;
    SerializeMap::end(map_ser)?;
    Ok(bytes)
}

fn encode_trace<T: TraceData, S: Serializer>(
    ser: S,
    chunk: &[Span<T>],
    metadata: &TracerMetadata,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;

    // Per-trace metadata. Always include hostname; other fields when set.
    map.serialize_entry("hostname", &metadata.hostname)?;
    if !metadata.env.is_empty() {
        map.serialize_entry("env", &metadata.env)?;
    }
    if !metadata.language.is_empty() {
        map.serialize_entry("languageName", &metadata.language)?;
    }
    if !metadata.language_version.is_empty() {
        map.serialize_entry("languageVersion", &metadata.language_version)?;
    }
    if !metadata.tracer_version.is_empty() {
        map.serialize_entry("tracerVersion", &metadata.tracer_version)?;
    }
    if !metadata.runtime_id.is_empty() {
        map.serialize_entry("runtimeID", &metadata.runtime_id)?;
    }
    if let Some(container_id) = libdd_common::entity_id::get_container_id() {
        map.serialize_entry("containerID", container_id)?;
    }

    map.serialize_entry(
        "spans",
        &ser_fn!(<T: TraceData> |ser, chunk: &'a [Span<T>]| {
            let mut seq = ser.serialize_seq(Some(chunk.len()))?;
            for (i, span) in chunk.iter().enumerate() {
                let is_first = i == 0;
                seq.serialize_element(&ser_fn!(<T: TraceData> |ser, span: &'a Span<T>, is_first: bool| {
                    encode_span(ser, span, is_first)
                }))?;
            }
            seq.end()
        }),
    )?;

    map.end()
}

fn encode_span<T: TraceData, S: Serializer>(
    ser: S,
    span: &Span<T>,
    is_first_in_trace: bool,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;

    let trace_id = span.trace_id;
    map.serialize_entry(
        "trace_id",
        &ser_fn!(|ser, trace_id: u128| {
            ser.collect_str(&format_args!("{:016x}", trace_id as u64))
        }),
    )?;
    let span_id = span.span_id;
    map.serialize_entry(
        "span_id",
        &ser_fn!(|ser, span_id: u64| { ser.collect_str(&format_args!("{:016x}", span_id as u64)) }),
    )?;
    let parent_id = span.parent_id;
    map.serialize_entry(
        "parent_id",
        &ser_fn!(|ser, parent_id: u64| {
            ser.collect_str(&format_args!("{:016x}", parent_id as u64))
        }),
    )?;

    // Resource defaults to name when empty.
    let name_str: &str = span.name.borrow();
    let resource_str: &str = span.resource.borrow();
    let service_str: &str = span.service.borrow();
    map.serialize_entry("name", name_str)?;
    map.serialize_entry(
        "resource",
        if resource_str.is_empty() {
            name_str
        } else {
            resource_str
        },
    )?;
    map.serialize_entry("service", service_str)?;
    map.serialize_entry("error", &span.error)?;
    map.serialize_entry("start", &span.start.max(0))?;
    map.serialize_entry("duration", &span.duration)?;

    let type_str: &str = span.r#type.borrow();
    if !type_str.is_empty() {
        map.serialize_entry("type", type_str)?;
    }

    map.serialize_entry(
        "meta",
        &ser_fn!(<T: TraceData> |ser, span: &'a Span<T>, is_first_in_trace: bool| {
            let upper_bits = (span.trace_id >> 64) as u64;
            let mut p_tid_seen = false;
            let mut span_links_seen = false;
            let mut events_seen = false;
            let mut compute_stats_seen = false;

            let mut meta = ser.serialize_map(None)?;
            for (k, v) in span.meta.iter() {
                let key: &str = k.borrow();
                match key {
                    "_dd.p.tid" => p_tid_seen = true,
                    "_dd.span_links" => span_links_seen = true,
                    "events" => events_seen = true,
                    "_dd.compute_stats"=> compute_stats_seen = true,
                    _ => {}
                };
                let val: &str = v.borrow();
                meta.serialize_entry(key, val)?;
            }
            if !p_tid_seen && upper_bits != 0 {
                meta.serialize_entry(
                    "_dd.p.tid",
                    &ser_fn!(|ser, upper_bits: u64| {
                        ser.collect_str(&format_args!("{:016x}", upper_bits as u64))
                    }),
                )?;
            }
            if !span_links_seen && !span.span_links.is_empty() {
                if let Some(s) = serialize_span_links(&span.span_links) {
                    meta.serialize_entry("_dd.span_links", &s)?;
                }
            }
            if !events_seen && !span.span_events.is_empty() {
                if let Some(s) = serialize_span_events(&span.span_events) {
                    meta.serialize_entry("events", &s)?;
                }
            }
            if !compute_stats_seen && is_first_in_trace {
                meta.serialize_entry("_dd.compute_stats", "1")?;
            }
            meta.end()
        }),
    )?;

    map.serialize_entry(
        "metrics",
        &ser_fn!(<T: TraceData> |ser, span: &'a Span<T>| {
            let mut metrics = ser.serialize_map(None)?;
            let mut trace_root_seen = false;
            for (k, v) in span.metrics.iter() {
                let key: &str = k.borrow();
                // serde_json refuses to serialize NaN/Inf; drop them silently.
                if v.is_finite() {
                    match key {
                        "_trace_root" => trace_root_seen = true,
                        "_top_level" => {
                            metrics.serialize_entry(key, &(*v as u32))?;
                            continue
                        },
                        _ => {},
                    }
                    metrics.serialize_entry(key, v)?
                }
            }
            if !trace_root_seen && span.parent_id == 0 {
                metrics.serialize_entry("_trace_root", &1u32)?;
            }
            metrics.end()
        }),
    )?;

    if !span.meta_struct.is_empty() {
        map.serialize_entry(
            "meta_struct",
            &ser_fn!(<T: TraceData> |ser, span: &'a Span<T>| {
                let mut ms = ser.serialize_map(None)?;
                for (k, v) in span.meta_struct.iter() {
                    let key: &str = k.borrow();
                    let bytes: &[u8] = v.borrow();

                    // abort whole payload on malformed entry
                    ms.serialize_entry(key, &MsgpackAsJson(bytes))?;
                }
                ms.end()
            }),
        )?;
    }
    map.end()
}

/// Serialize span links to a JSON string suitable for `meta['_dd.span_links']`.
///
/// Returns `None` if serialization fails. The result is truncated to
/// [`MAX_META_VALUE_LEN`] characters with a trailing `"..."` if it would
/// otherwise exceed that limit.
fn serialize_span_links<T: TraceData>(links: &[SpanLink<T>]) -> Option<String> {
    let s = serde_json::to_string(&ser_fn!(<T: TraceData> |ser, links: &'a [SpanLink<T>]| {
        let mut seq = ser.serialize_seq(Some(links.len()))?;
        for link in links {
            seq.serialize_element(&ser_fn!(<T: TraceData> |ser, link: &'a SpanLink<T>| {
                encode_span_link(ser, link)
            }))?;
        }
        seq.end()
    }))
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

fn encode_span_link<T: TraceData, S: Serializer>(
    ser: S,
    link: &SpanLink<T>,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    let trace_id_128: u128 = ((link.trace_id_high as u128) << 64) | (link.trace_id as u128);
    map.serialize_entry("trace_id", &format!("{:032x}", trace_id_128))?;
    map.serialize_entry("span_id", &format!("{:016x}", link.span_id))?;
    if !link.attributes.is_empty() {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: TraceData> |ser, link: &'a SpanLink<T>| {
                let mut attrs = ser.serialize_map(Some(link.attributes.len()))?;
                for (k, v) in link.attributes.iter() {
                    let key: &str = k.borrow();
                    let val: &str = v.borrow();
                    attrs.serialize_entry(key, val)?;
                }
                attrs.end()
            }),
        )?;
    }
    // When `flags` is 0, no sampling decision exists, so omit the field. Before emission,
    // mask off the internal "explicitly set" sentinel (bit 31), because this JSON field uses
    // the same `_dd.span_links` key that the v0.5 encoder produces and must match its output.
    if link.flags != 0 {
        map.serialize_entry(
            "flags",
            &((link.flags & !SPAN_LINK_FLAGS_SET_SENTINEL) as u64),
        )?;
    }
    let tracestate: &str = link.tracestate.borrow();
    if !tracestate.is_empty() {
        map.serialize_entry("tracestate", tracestate)?;
    }
    map.end()
}

/// Serialize span events to a JSON string suitable for `meta['events']`.
fn serialize_span_events<T: TraceData>(events: &[SpanEvent<T>]) -> Option<String> {
    let s = serde_json::to_string(&ser_fn!(<T: TraceData> |ser, events: &'a [SpanEvent<T>]| {
        let mut seq = ser.serialize_seq(Some(events.len()))?;
        for event in events {
            seq.serialize_element(&ser_fn!(<T: TraceData> |ser, event: &'a SpanEvent<T>| {
                encode_span_event(ser, event)
            }))?;
        }
        seq.end()
    }))
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

fn encode_span_event<T: TraceData, S: Serializer>(
    ser: S,
    event: &SpanEvent<T>,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    let name: &str = event.name.borrow();
    map.serialize_entry("name", name)?;
    map.serialize_entry("time_unix_nano", &event.time_unix_nano)?;
    if !event.attributes.is_empty() {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: TraceData> |ser, event: &'a SpanEvent<T>| {
                let mut attrs = ser.serialize_map(Some(event.attributes.len()))?;
                for (k, v) in event.attributes.iter() {
                    let key: &str = k.borrow();
                    attrs.serialize_entry(key, &ser_fn!(<T: TraceData> |ser, v: &'a AttributeAnyValue<T> | {
                        match v {
                            AttributeAnyValue::SingleValue(v) => serialize_scalar(ser, v),
                            AttributeAnyValue::Array(values) => {
                                let mut seq = ser.serialize_seq(Some(values.len()))?;
                                for v in values {
                                    seq.serialize_element(&ser_fn!(<T: TraceData> |ser, v: &'a AttributeArrayValue<T>| {
                                        serialize_scalar(ser, v)
                                    }))?;
                                }
                                seq.end()
                            }
                        }
                    }))?;
                }
                attrs.end()
            }),
        )?;
    }
    map.end()
}

fn serialize_scalar<S: serde::Serializer, T: TraceData>(
    ser: S,
    s: &AttributeArrayValue<T>,
) -> Result<S::Ok, S::Error> {
    match s {
        AttributeArrayValue::String(s) => {
            let s: &str = s.borrow();
            ser.serialize_str(s)
        }
        AttributeArrayValue::Boolean(b) => ser.serialize_bool(*b),
        AttributeArrayValue::Integer(i) => ser.serialize_i64(*i),
        AttributeArrayValue::Double(d) => {
            if d.is_finite() {
                ser.serialize_f64(*d)
            } else {
                // NaN/Inf become JSON null.
                ser.serialize_unit()
            }
        }
    }
}

/// Reserved v0.4 `meta`/`metrics` key names written from dedicated typed fields (`env`, chunk
/// `origin`, ...) rather than from the v1 attribute map — the dedicated field always wins and
/// a colliding attribute is dropped. See
/// [`crate::msgpack_encoder::v04::span_v1::PROMOTED_ATTR_KEYS`] for the sibling list on the
/// msgpack side (this one omits `_dd.p.tid`, which is handled below with "seen" tracking
/// instead, matching this module's own `_dd.p.tid`/`_dd.span_links`/`events` convention).
const PROMOTED_ATTR_KEYS_V1: &[&str] = &[
    "env",
    "version",
    "component",
    "span.kind",
    "_dd.origin",
    "_dd.p.dm",
    "_sampling_priority_v1",
];

/// Maps a `SpanKind` to its v0.4 `span.kind` meta string. Returns `None` for `Internal` so
/// callers can skip emitting the default value.
fn span_kind_to_meta_v1(kind: SpanKind) -> Option<&'static str> {
    match kind {
        SpanKind::Internal => None,
        SpanKind::Server => Some("server"),
        SpanKind::Client => Some("client"),
        SpanKind::Producer => Some("producer"),
        SpanKind::Consumer => Some("consumer"),
    }
}

/// Drops entries whose key was already seen, keeping the first occurrence: two distinct
/// attributes can flatten to the same dotted key.
fn dedup_first_wins_v1<V>(mut leaves: Vec<(String, V)>) -> Vec<(String, V)> {
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
/// `meta` (string-valued), `metrics` (numeric), and `meta_struct` (`Bytes`) buckets.
fn flatten_attr_into_v1<T: TraceData>(
    key: &mut String,
    v: &AttributeValueV1<T>,
    meta_out: &mut Vec<(String, String)>,
    metrics_out: &mut Vec<(String, f64)>,
    bytes_out: &mut Vec<(String, T::Bytes)>,
) {
    match v {
        AttributeValueV1::String(s) => meta_out.push((key.clone(), s.borrow().to_owned())),
        AttributeValueV1::Bool(b) => {
            meta_out.push((key.clone(), if *b { "true" } else { "false" }.to_owned()))
        }
        AttributeValueV1::Int(i) => metrics_out.push((key.clone(), *i as f64)),
        AttributeValueV1::Float(f) => metrics_out.push((key.clone(), *f)),
        AttributeValueV1::Bytes(b) => bytes_out.push((key.clone(), b.clone())),
        AttributeValueV1::List(items) => {
            let base_len = key.len();
            for (i, item) in items.iter().enumerate() {
                key.push('.');
                let _ = write!(key, "{i}");
                flatten_attr_into_v1(key, item, meta_out, metrics_out, bytes_out);
                key.truncate(base_len);
            }
        }
        AttributeValueV1::KeyValue(map) => {
            let base_len = key.len();
            for (k, v) in map.defensive_dedup().iter() {
                key.push('.');
                key.push_str(k.borrow());
                flatten_attr_into_v1(key, v, meta_out, metrics_out, bytes_out);
                key.truncate(base_len);
            }
        }
    }
}

/// Leaves collected by [`collect_attrs_v1`]: `meta`, `metrics`, and `meta_struct` (`Bytes`)
/// entries.
type CollectedAttrsV1<T> = (
    Vec<(String, String)>,
    Vec<(String, f64)>,
    Vec<(String, <T as TraceData>::Bytes)>,
);

/// Merges a span's attributes with its chunk's (span overrides chunk on key collision),
/// drops attributes colliding with a [`PROMOTED_ATTR_KEYS_V1`] name, and splits the rest into
/// `meta` leaves, `metrics` leaves, and `meta_struct` (`Bytes`) leaves.
fn collect_attrs_v1<T: TraceData>(span: &SpanV1<T>, chunk: &TraceChunk<T>) -> CollectedAttrsV1<T> {
    let span_attrs_dd = span.attributes.defensive_dedup();
    let chunk_attrs_dd = chunk.attributes.defensive_dedup();
    let merged_attrs = span_attrs_dd
        .iter()
        .filter(|(k, _)| !PROMOTED_ATTR_KEYS_V1.contains(&(*k).borrow()))
        .chain(chunk_attrs_dd.iter().filter(|(k, _)| {
            !PROMOTED_ATTR_KEYS_V1.contains(&(*k).borrow())
                && !span_attrs_dd.iter().any(|(k2, _)| k2 == *k)
        }));

    let mut meta_leaves: Vec<(String, String)> = Vec::new();
    let mut metrics_leaves: Vec<(String, f64)> = Vec::new();
    let mut bytes_leaves: Vec<(String, T::Bytes)> = Vec::new();
    let mut key_buf = String::new();
    for (k, v) in merged_attrs {
        key_buf.clear();
        key_buf.push_str(k.borrow());
        flatten_attr_into_v1(
            &mut key_buf,
            v,
            &mut meta_leaves,
            &mut metrics_leaves,
            &mut bytes_leaves,
        );
    }
    (
        dedup_first_wins_v1(meta_leaves),
        dedup_first_wins_v1(metrics_leaves),
        dedup_first_wins_v1(bytes_leaves),
    )
}

/// V1-native analog of [`encode_payload`]. Downgrades v1's unified attribute model back to the
/// same `meta`/`metrics`/`meta_struct`-shaped wire fields, so the emitted JSON body is
/// equivalent to what a v0.4 tracer would produce for the same trace — see
/// [`crate::msgpack_encoder::v04::span_v1`] for the mapping table this mirrors. Chunk-level
/// context (`trace_id`, `origin`, `priority`, `sampling_mechanism`, `dropped_trace`, chunk
/// attributes) is propagated into every span, matching the [`TraceChunk`]-level granularity v1
/// operates at.
pub fn encode_payload_from_v1<T: TraceData>(
    chunks: &[TraceChunk<T>],
    metadata: &TracerMetadata,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut bytes);

    let mut map_ser = serializer.serialize_map(Some(1))?;
    map_ser.serialize_entry(
        "traces",
        &ser_fn!(<T: TraceData> |ser, chunks: &'a [TraceChunk<T>], metadata: &'a TracerMetadata| {
            let mut traces_serializer = ser.serialize_seq(Some(chunks.len()))?;
            for chunk in chunks {
                traces_serializer.serialize_element(&ser_fn!(<T: TraceData> |ser, chunk: &'a TraceChunk<T>, metadata: &'a TracerMetadata| {
                    encode_trace_v1(ser, chunk, metadata)
                }))?;
            }
            traces_serializer.end()
        }),
    )?;
    SerializeMap::end(map_ser)?;
    Ok(bytes)
}

fn encode_trace_v1<T: TraceData, S: Serializer>(
    ser: S,
    chunk: &TraceChunk<T>,
    metadata: &TracerMetadata,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;

    map.serialize_entry("hostname", &metadata.hostname)?;
    if !metadata.env.is_empty() {
        map.serialize_entry("env", &metadata.env)?;
    }
    if !metadata.language.is_empty() {
        map.serialize_entry("languageName", &metadata.language)?;
    }
    if !metadata.language_version.is_empty() {
        map.serialize_entry("languageVersion", &metadata.language_version)?;
    }
    if !metadata.tracer_version.is_empty() {
        map.serialize_entry("tracerVersion", &metadata.tracer_version)?;
    }
    if !metadata.runtime_id.is_empty() {
        map.serialize_entry("runtimeID", &metadata.runtime_id)?;
    }
    if let Some(container_id) = libdd_common::entity_id::get_container_id() {
        map.serialize_entry("containerID", container_id)?;
    }

    map.serialize_entry(
        "spans",
        &ser_fn!(<T: TraceData> |ser, chunk: &'a TraceChunk<T>| {
            let mut seq = ser.serialize_seq(Some(chunk.spans.len()))?;
            for (i, span) in chunk.spans.iter().enumerate() {
                let is_first = i == 0;
                seq.serialize_element(&ser_fn!(<T: TraceData> |ser, chunk: &'a TraceChunk<T>, span: &'a SpanV1<T>, is_first: bool| {
                    encode_span_v1(ser, chunk, span, is_first)
                }))?;
            }
            seq.end()
        }),
    )?;

    map.end()
}

fn encode_span_v1<T: TraceData, S: Serializer>(
    ser: S,
    chunk: &TraceChunk<T>,
    span: &SpanV1<T>,
    is_first_in_trace: bool,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;

    let mut trace_id_low_bytes = [0u8; 8];
    let mut trace_id_high_bytes = [0u8; 8];
    trace_id_low_bytes.copy_from_slice(&chunk.trace_id[8..16]);
    trace_id_high_bytes.copy_from_slice(&chunk.trace_id[0..8]);
    let trace_id_low = u64::from_be_bytes(trace_id_low_bytes);
    let trace_id_high = u64::from_be_bytes(trace_id_high_bytes);
    map.serialize_entry(
        "trace_id",
        &ser_fn!(|ser, trace_id_low: u64| {
            ser.collect_str(&format_args!("{trace_id_low:016x}"))
        }),
    )?;
    let span_id = span.span_id;
    map.serialize_entry(
        "span_id",
        &ser_fn!(|ser, span_id: u64| { ser.collect_str(&format_args!("{span_id:016x}")) }),
    )?;
    let parent_id = span.parent_id;
    map.serialize_entry(
        "parent_id",
        &ser_fn!(|ser, parent_id: u64| { ser.collect_str(&format_args!("{parent_id:016x}")) }),
    )?;

    // Resource defaults to name when empty.
    let name_str: &str = span.name.borrow();
    let resource_str: &str = span.resource.borrow();
    let service_str: &str = span.service.borrow();
    map.serialize_entry("name", name_str)?;
    map.serialize_entry(
        "resource",
        if resource_str.is_empty() {
            name_str
        } else {
            resource_str
        },
    )?;
    map.serialize_entry("service", service_str)?;
    // v0.4's `error` is emitted as an integer (0/1) on the wire, unlike v1's own `bool` field.
    map.serialize_entry("error", &(span.error as i32))?;
    map.serialize_entry("start", &span.start.max(0))?;
    map.serialize_entry("duration", &span.duration)?;

    let type_str: &str = span.r#type.borrow();
    if !type_str.is_empty() {
        map.serialize_entry("type", type_str)?;
    }

    let (meta_leaves, metrics_leaves, bytes_attrs) = collect_attrs_v1(span, chunk);
    let meta_leaves = &meta_leaves;
    let metrics_leaves = &metrics_leaves;
    let bytes_attrs = &bytes_attrs;
    let priority = if chunk.dropped_trace {
        // v0.4 has no wire-level equivalent of `dropped_trace`; force `USER_REJECT` (-1)
        // unless the chunk already carries a negative (reject-like) priority — same
        // convention as the msgpack downgrade encoder.
        Some(chunk.priority.filter(|&p| p < 0).unwrap_or(-1))
    } else {
        chunk.priority
    };

    map.serialize_entry(
        "meta",
        &ser_fn!(<T: TraceData> |ser, span: &'a SpanV1<T>, chunk: &'a TraceChunk<T>, meta_leaves: &'a Vec<(String, String)>, is_first_in_trace: bool, trace_id_high: u64| {
            let mut meta = ser.serialize_map(None)?;

            let env: &str = span.env.borrow();
            if !env.is_empty() {
                meta.serialize_entry("env", env)?;
            }
            let version: &str = span.version.borrow();
            if !version.is_empty() {
                meta.serialize_entry("version", version)?;
            }
            let component: &str = span.component.borrow();
            if !component.is_empty() {
                meta.serialize_entry("component", component)?;
            }
            if let Some(kind) = span_kind_to_meta_v1(span.span_kind) {
                meta.serialize_entry("span.kind", kind)?;
            }
            let origin: &str = chunk.origin.borrow();
            if !origin.is_empty() {
                meta.serialize_entry("_dd.origin", origin)?;
            }
            if let Some(mechanism) = chunk.sampling_mechanism {
                let mut buf = itoa::Buffer::new();
                meta.serialize_entry("_dd.p.dm", buf.format(-(mechanism as i64)))?;
            }

            let mut p_tid_seen = false;
            let mut span_links_seen = false;
            let mut events_seen = false;
            let mut compute_stats_seen = false;
            for (key, val) in meta_leaves.iter() {
                match key.as_str() {
                    "_dd.p.tid" => p_tid_seen = true,
                    "_dd.span_links" => span_links_seen = true,
                    "events" => events_seen = true,
                    "_dd.compute_stats" => compute_stats_seen = true,
                    _ => {}
                };
                meta.serialize_entry(key, val)?;
            }
            if !p_tid_seen && trace_id_high != 0 {
                meta.serialize_entry(
                    "_dd.p.tid",
                    &ser_fn!(|ser, trace_id_high: u64| {
                        ser.collect_str(&format_args!("{trace_id_high:016x}"))
                    }),
                )?;
            }
            if !span_links_seen && !span.span_links.is_empty() {
                if let Some(s) = serialize_span_links_v1(&span.span_links) {
                    meta.serialize_entry("_dd.span_links", &s)?;
                }
            }
            if !events_seen && !span.span_events.is_empty() {
                if let Some(s) = serialize_span_events_v1(&span.span_events) {
                    meta.serialize_entry("events", &s)?;
                }
            }
            if !compute_stats_seen && is_first_in_trace {
                meta.serialize_entry("_dd.compute_stats", "1")?;
            }
            meta.end()
        }),
    )?;

    map.serialize_entry(
        "metrics",
        &ser_fn!(<T: TraceData> |ser, span: &'a SpanV1<T>, metrics_leaves: &'a Vec<(String, f64)>, priority: Option<i32>| {
            let mut metrics = ser.serialize_map(None)?;
            let mut trace_root_seen = false;
            for (key, val) in metrics_leaves.iter() {
                match key.as_str() {
                    "_trace_root" => trace_root_seen = true,
                    "_top_level" => {
                        metrics.serialize_entry(key, &(*val as u32))?;
                        continue;
                    }
                    _ => {}
                }
                metrics.serialize_entry(key, val)?;
            }
            if let Some(p) = priority {
                metrics.serialize_entry("_sampling_priority_v1", &(p as f64))?;
            }
            if !trace_root_seen && span.parent_id == 0 {
                metrics.serialize_entry("_trace_root", &1u32)?;
            }
            metrics.end()
        }),
    )?;

    if !bytes_attrs.is_empty() {
        map.serialize_entry(
            "meta_struct",
            &ser_fn!(<T: TraceData> |ser, span: &'a SpanV1<T>, bytes_attrs: &'a Vec<(String, T::Bytes)>| {
                let _ = span;
                let mut ms = ser.serialize_map(None)?;
                for (k, v) in bytes_attrs.iter() {
                    let raw: &[u8] = v.borrow();
                    ms.serialize_entry(k, &MsgpackAsJson(raw))?;
                }
                ms.end()
            }),
        )?;
    }
    map.end()
}

/// Serialize v1 span links to a JSON string suitable for `meta['_dd.span_links']`. Same
/// truncation convention as [`serialize_span_links`].
fn serialize_span_links_v1<T: TraceData>(links: &[SpanLinkV1<T>]) -> Option<String> {
    let s = serde_json::to_string(&ser_fn!(<T: TraceData> |ser, links: &'a [SpanLinkV1<T>]| {
        let mut seq = ser.serialize_seq(Some(links.len()))?;
        for link in links {
            seq.serialize_element(&ser_fn!(<T: TraceData> |ser, link: &'a SpanLinkV1<T>| {
                encode_span_link_v1(ser, link)
            }))?;
        }
        seq.end()
    }))
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

fn encode_span_link_v1<T: TraceData, S: Serializer>(
    ser: S,
    link: &SpanLinkV1<T>,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    let trace_id_128 = u128::from_be_bytes(link.trace_id);
    map.serialize_entry("trace_id", &format!("{trace_id_128:032x}"))?;
    map.serialize_entry("span_id", &format!("{:016x}", link.span_id))?;
    let attrs_dd = link.attributes.defensive_dedup();
    let attrs_dd = &attrs_dd;
    let has_attributes = attrs_dd
        .iter()
        .any(|(_, v)| matches!(v, AttributeValueV1::String(_) | AttributeValueV1::Bool(_)));
    if has_attributes {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: TraceData> |ser, attrs_dd: &'a crate::span::vec_map::DedupedVecMap<'a, T::Text, AttributeValueV1<T>>| {
                let mut attrs = ser.serialize_map(None)?;
                for (k, v) in attrs_dd.iter() {
                    let key: &str = k.borrow();
                    match v {
                        AttributeValueV1::String(s) => attrs.serialize_entry(key, s.borrow() as &str)?,
                        AttributeValueV1::Bool(b) => {
                            attrs.serialize_entry(key, if *b { "true" } else { "false" })?
                        }
                        _ => {}
                    }
                }
                attrs.end()
            }),
        )?;
    }
    // When `flags` is 0, no sampling decision exists, so omit the field. Mask off the internal
    // "explicitly set" sentinel (bit 31) before emission, same as `encode_span_link`.
    if link.flags != 0 {
        map.serialize_entry(
            "flags",
            &((link.flags & !SPAN_LINK_FLAGS_SET_SENTINEL) as u64),
        )?;
    }
    let tracestate: &str = link.tracestate.borrow();
    if !tracestate.is_empty() {
        map.serialize_entry("tracestate", tracestate)?;
    }
    map.end()
}

/// Serialize v1 span events to a JSON string suitable for `meta['events']`. Same truncation
/// convention as [`serialize_span_events`].
fn serialize_span_events_v1<T: TraceData>(events: &[SpanEventV1<T>]) -> Option<String> {
    let s = serde_json::to_string(
        &ser_fn!(<T: TraceData> |ser, events: &'a [SpanEventV1<T>]| {
            let mut seq = ser.serialize_seq(Some(events.len()))?;
            for event in events {
                seq.serialize_element(&ser_fn!(<T: TraceData> |ser, event: &'a SpanEventV1<T>| {
                    encode_span_event_v1(ser, event)
                }))?;
            }
            seq.end()
        }),
    )
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

/// Returns `true` when `v` can be downgraded to a v0.4 event-attribute (scalar or scalar list).
fn is_supported_event_attr_v1<T: TraceData>(v: &AttributeValueV1<T>) -> bool {
    matches!(
        v,
        AttributeValueV1::String(_)
            | AttributeValueV1::Bool(_)
            | AttributeValueV1::Int(_)
            | AttributeValueV1::Float(_)
            | AttributeValueV1::List(_)
    )
}

/// Returns `true` when `v` is a scalar that fits in a v0.4 array element (no nesting).
fn is_scalar_array_elem_v1<T: TraceData>(v: &AttributeValueV1<T>) -> bool {
    matches!(
        v,
        AttributeValueV1::String(_)
            | AttributeValueV1::Bool(_)
            | AttributeValueV1::Int(_)
            | AttributeValueV1::Float(_)
    )
}

fn encode_span_event_v1<T: TraceData, S: Serializer>(
    ser: S,
    event: &SpanEventV1<T>,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    let name: &str = event.name.borrow();
    map.serialize_entry("name", name)?;
    map.serialize_entry("time_unix_nano", &event.time_unix_nano)?;
    let attrs_dd = event.attributes.defensive_dedup();
    let attrs_dd = &attrs_dd;
    let has_attributes = attrs_dd.iter().any(|(_, v)| is_supported_event_attr_v1(v));
    if has_attributes {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: TraceData> |ser, attrs_dd: &'a crate::span::vec_map::DedupedVecMap<'a, T::Text, AttributeValueV1<T>>| {
                let mut attrs = ser.serialize_map(None)?;
                for (k, v) in attrs_dd.iter().filter(|(_, v)| is_supported_event_attr_v1(v)) {
                    let key: &str = k.borrow();
                    attrs.serialize_entry(key, &ser_fn!(<T: TraceData> |ser, v: &'a AttributeValueV1<T>| {
                        encode_event_attr_value_v1(ser, v)
                    }))?;
                }
                attrs.end()
            }),
        )?;
    }
    map.end()
}

/// Serializes a v1 event attribute value in the v0.4 `{"type": <u8>, "<kind>_value": ...}`
/// shape. `List` produces `{"type": 4, "array_value": {"values": [...]}}`, filtering nested
/// entries out of the array (no v0.4 array-element equivalent for them).
fn encode_event_attr_value_v1<T: TraceData, S: Serializer>(
    ser: S,
    v: &AttributeValueV1<T>,
) -> Result<S::Ok, S::Error> {
    match v {
        AttributeValueV1::List(items) => {
            let mut map = ser.serialize_map(Some(2))?;
            map.serialize_entry("type", &4u8)?;
            map.serialize_entry(
                "array_value",
                &ser_fn!(<T: TraceData> |ser, items: &'a Vec<AttributeValueV1<T>>| {
                    let mut m = ser.serialize_map(Some(1))?;
                    m.serialize_entry(
                        "values",
                        &ser_fn!(<T: TraceData> |ser, items: &'a Vec<AttributeValueV1<T>>| {
                            let scalars: Vec<_> = items.iter().filter(|e| is_scalar_array_elem_v1(e)).collect();
                            let mut seq = ser.serialize_seq(Some(scalars.len()))?;
                            for elem in scalars {
                                seq.serialize_element(&ser_fn!(<T: TraceData> |ser, elem: &'a AttributeValueV1<T>| {
                                    encode_event_scalar_v1(ser, elem)
                                }))?;
                            }
                            seq.end()
                        }),
                    )?;
                    m.end()
                }),
            )?;
            map.end()
        }
        other => encode_event_scalar_v1(ser, other),
    }
}

fn encode_event_scalar_v1<T: TraceData, S: Serializer>(
    ser: S,
    v: &AttributeValueV1<T>,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(Some(2))?;
    match v {
        AttributeValueV1::String(s) => {
            map.serialize_entry("type", &0u8)?;
            map.serialize_entry("string_value", s.borrow() as &str)?;
        }
        AttributeValueV1::Bool(b) => {
            map.serialize_entry("type", &1u8)?;
            map.serialize_entry("bool_value", b)?;
        }
        AttributeValueV1::Int(i) => {
            map.serialize_entry("type", &2u8)?;
            map.serialize_entry("int_value", i)?;
        }
        AttributeValueV1::Float(f) => {
            map.serialize_entry("type", &3u8)?;
            map.serialize_entry("double_value", f)?;
        }
        _ => unreachable!("filtered by is_scalar_array_elem_v1"),
    }
    map.end()
}

/// `serde::Serialize` adapter that interprets `bytes` as a self-describing
/// msgpack value and transcodes it into the destination serializer.
///
/// Used to inline `meta_struct` values (which are stored as msgpack-encoded
/// bytes) into the agentless JSON payload as real JSON objects.
struct MsgpackAsJson<'a>(&'a [u8]);

impl serde::Serialize for MsgpackAsJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut de = rmp_serde::Deserializer::from_read_ref(self.0);
        serde_transcode::transcode(&mut de, serializer)
    }
}

/// Truncate `s` to at most `max_len` bytes, appending `"..."` when truncation occurs.
fn truncate_with_ellipsis(mut s: String, max_len: usize) -> String {
    if s.len() <= max_len {
        return s;
    }
    let suffix_len = TRUNCATION_SUFFIX.len();
    let cut = max_len.saturating_sub(suffix_len);
    // Find the previous char boundary so we don't slice in the middle of a UTF-8 sequence.
    let mut end = cut;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str(TRUNCATION_SUFFIX);
    s
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_v1;
