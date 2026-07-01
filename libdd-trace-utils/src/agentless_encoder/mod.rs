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
use crate::span::TraceData;
use crate::tracer_metadata::TracerMetadata;
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Serializer,
};
use std::borrow::Borrow;

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
    // `flags == 0` means no sampling decision is available; omit the field.
    if link.flags != 0 {
        map.serialize_entry("flags", &(link.flags as u64))?;
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
