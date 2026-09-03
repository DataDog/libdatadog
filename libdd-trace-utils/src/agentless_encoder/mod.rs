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
use crate::span::{TraceData, SPAN_LINK_FLAGS_SET_SENTINEL};
use crate::tracer_metadata::TracerMetadata;
use serde::{
    ser::{Error as _, SerializeMap, SerializeSeq},
    Serialize, Serializer,
};
use std::borrow::Borrow;

/// Maximum allowed size of a `meta` value before truncation.
const MAX_META_VALUE_LEN: usize = 25_000;
/// Suffix appended when a `meta` value is truncated.
const TRUNCATION_SUFFIX: &str = "...";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

struct FixedHex<const N: usize>([u8; N]);

impl<const N: usize> serde::Serialize for FixedHex<N> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = std::str::from_utf8(&self.0).map_err(S::Error::custom)?;
        serializer.serialize_str(value)
    }
}

fn fixed_hex<const N: usize>(mut value: u128) -> FixedHex<N> {
    let mut encoded = [b'0'; N];
    let mut index = N;

    while index != 0 {
        index -= 1;
        // The mask guarantees that the conversion fits and indexes HEX_DIGITS.
        encoded[index] = HEX_DIGITS[(value & 0x0f) as usize];
        value >>= 4;
    }

    FixedHex(encoded)
}

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
/// When `client_side_stats` is `true`, the encoder will **not** inject
/// `meta["_dd.compute_stats"]="1"` on the first span of each chunk.  Set this
/// when the caller is already computing and exporting stats locally so that the intake does not
/// double- count the same traces.
///
/// Returns the serialized JSON bytes on success.
pub fn encode_payload<T: TraceData>(
    traces: &[Vec<Span<T>>],
    metadata: &TracerMetadata,
    client_side_stats: bool,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(br#"{"traces":["#);
    for (index, chunk) in traces.iter().enumerate() {
        if index != 0 {
            bytes.push(b',');
        }
        encode_trace(&mut bytes, chunk, metadata, client_side_stats)?;
    }
    bytes.extend_from_slice(b"]}");
    Ok(bytes)
}

#[inline]
fn serialize_json<T: ?Sized + Serialize>(
    bytes: &mut Vec<u8>,
    value: &T,
) -> Result<(), serde_json::Error> {
    value.serialize(&mut serde_json::Serializer::new(bytes))
}

#[inline]
fn write_entry_separator(bytes: &mut Vec<u8>, first: &mut bool) {
    if *first {
        *first = false;
    } else {
        bytes.push(b',');
    }
}

#[inline]
fn serialize_fixed_entry<T: ?Sized + Serialize>(
    bytes: &mut Vec<u8>,
    first: &mut bool,
    key: &[u8],
    value: &T,
) -> Result<(), serde_json::Error> {
    write_entry_separator(bytes, first);
    bytes.extend_from_slice(key);
    serialize_json(bytes, value)
}

#[inline]
fn serialize_dynamic_entry<T: ?Sized + Serialize>(
    bytes: &mut Vec<u8>,
    first: &mut bool,
    key: &str,
    value: &T,
) -> Result<(), serde_json::Error> {
    write_entry_separator(bytes, first);
    serialize_json(bytes, key)?;
    bytes.push(b':');
    serialize_json(bytes, value)
}

fn encode_trace<T: TraceData>(
    bytes: &mut Vec<u8>,
    chunk: &[Span<T>],
    metadata: &TracerMetadata,
    client_side_stats: bool,
) -> Result<(), serde_json::Error> {
    bytes.push(b'{');
    let mut first = true;

    // Per-trace metadata. Always include hostname; other fields when set.
    serialize_fixed_entry(bytes, &mut first, br#""hostname":"#, &metadata.hostname)?;
    if !metadata.env.is_empty() {
        serialize_fixed_entry(bytes, &mut first, br#""env":"#, &metadata.env)?;
    }
    if !metadata.language.is_empty() {
        serialize_fixed_entry(bytes, &mut first, br#""languageName":"#, &metadata.language)?;
    }
    if !metadata.language_version.is_empty() {
        serialize_fixed_entry(
            bytes,
            &mut first,
            br#""languageVersion":"#,
            &metadata.language_version,
        )?;
    }
    if !metadata.tracer_version.is_empty() {
        serialize_fixed_entry(
            bytes,
            &mut first,
            br#""tracerVersion":"#,
            &metadata.tracer_version,
        )?;
    }
    if !metadata.runtime_id.is_empty() {
        serialize_fixed_entry(bytes, &mut first, br#""runtimeID":"#, &metadata.runtime_id)?;
    }
    if let Some(container_id) = libdd_common::entity_id::get_container_id() {
        serialize_fixed_entry(bytes, &mut first, br#""containerID":"#, container_id)?;
    }

    write_entry_separator(bytes, &mut first);
    bytes.extend_from_slice(br#""spans":["#);
    for (index, span) in chunk.iter().enumerate() {
        if index != 0 {
            bytes.push(b',');
        }
        encode_span(bytes, span, index == 0, client_side_stats)?;
    }
    bytes.extend_from_slice(b"]}");
    Ok(())
}

fn encode_span<T: TraceData>(
    bytes: &mut Vec<u8>,
    span: &Span<T>,
    is_first_in_trace: bool,
    client_side_stats: bool,
) -> Result<(), serde_json::Error> {
    bytes.push(b'{');
    let mut first = true;

    serialize_fixed_entry(
        bytes,
        &mut first,
        br#""trace_id":"#,
        &fixed_hex::<16>(span.trace_id),
    )?;
    serialize_fixed_entry(
        bytes,
        &mut first,
        br#""span_id":"#,
        &fixed_hex::<16>(u128::from(span.span_id)),
    )?;
    serialize_fixed_entry(
        bytes,
        &mut first,
        br#""parent_id":"#,
        &fixed_hex::<16>(u128::from(span.parent_id)),
    )?;

    // Resource defaults to name when empty.
    let name_str: &str = span.name.borrow();
    let resource_str: &str = span.resource.borrow();
    let service_str: &str = span.service.borrow();
    serialize_fixed_entry(bytes, &mut first, br#""name":"#, name_str)?;
    serialize_fixed_entry(
        bytes,
        &mut first,
        br#""resource":"#,
        if resource_str.is_empty() {
            name_str
        } else {
            resource_str
        },
    )?;
    serialize_fixed_entry(bytes, &mut first, br#""service":"#, service_str)?;
    serialize_fixed_entry(bytes, &mut first, br#""error":"#, &span.error)?;
    serialize_fixed_entry(bytes, &mut first, br#""start":"#, &span.start.max(0))?;
    serialize_fixed_entry(bytes, &mut first, br#""duration":"#, &span.duration)?;

    let type_str: &str = span.r#type.borrow();
    if !type_str.is_empty() {
        serialize_fixed_entry(bytes, &mut first, br#""type":"#, type_str)?;
    }

    write_entry_separator(bytes, &mut first);
    bytes.extend_from_slice(br#""meta":{"#);
    let mut first_meta = true;
    let upper_bits = (span.trace_id >> 64) as u64;
    let mut p_tid_seen = false;
    let mut span_links_seen = false;
    let mut events_seen = false;
    let mut compute_stats_seen = false;

    for (key, value) in span.meta.defensive_dedup().iter() {
        let key: &str = key.borrow();
        match key {
            "_dd.p.tid" => p_tid_seen = true,
            "_dd.span_links" => span_links_seen = true,
            "events" => events_seen = true,
            "_dd.compute_stats" => compute_stats_seen = true,
            _ => {}
        };
        let value: &str = value.borrow();
        serialize_dynamic_entry(bytes, &mut first_meta, key, value)?;
    }
    if !p_tid_seen && upper_bits != 0 {
        serialize_fixed_entry(
            bytes,
            &mut first_meta,
            br#""_dd.p.tid":"#,
            &fixed_hex::<16>(u128::from(upper_bits)),
        )?;
    }
    if !span_links_seen && !span.span_links.is_empty() {
        if let Some(value) = serialize_span_links(&span.span_links) {
            serialize_fixed_entry(bytes, &mut first_meta, br#""_dd.span_links":"#, &value)?;
        }
    }
    if !events_seen && !span.span_events.is_empty() {
        if let Some(value) = serialize_span_events(&span.span_events) {
            serialize_fixed_entry(bytes, &mut first_meta, br#""events":"#, &value)?;
        }
    }
    if !compute_stats_seen && is_first_in_trace && !client_side_stats {
        serialize_fixed_entry(bytes, &mut first_meta, br#""_dd.compute_stats":"#, "1")?;
    }
    bytes.push(b'}');

    write_entry_separator(bytes, &mut first);
    bytes.extend_from_slice(br#""metrics":{"#);
    let mut first_metric = true;
    let mut trace_root_seen = false;
    for (key, value) in span.metrics.defensive_dedup().iter() {
        let key: &str = key.borrow();
        // serde_json refuses to serialize NaN/Inf; drop them silently.
        if value.is_finite() {
            match key {
                "_trace_root" => trace_root_seen = true,
                "_top_level" => {
                    serialize_dynamic_entry(bytes, &mut first_metric, key, &(*value as u32))?;
                    continue;
                }
                _ => {}
            }
            serialize_dynamic_entry(bytes, &mut first_metric, key, value)?;
        }
    }
    if !trace_root_seen && span.parent_id == 0 {
        serialize_fixed_entry(bytes, &mut first_metric, br#""_trace_root":"#, &1u32)?;
    }
    bytes.push(b'}');

    if !span.meta_struct.is_empty() {
        write_entry_separator(bytes, &mut first);
        bytes.extend_from_slice(br#""meta_struct":{"#);
        let mut first_meta_struct = true;
        for (key, value) in span.meta_struct.iter() {
            let key: &str = key.borrow();
            let value: &[u8] = value.borrow();

            // Abort the whole payload on a malformed entry.
            serialize_dynamic_entry(bytes, &mut first_meta_struct, key, &MsgpackAsJson(value))?;
        }
        bytes.push(b'}');
    }
    bytes.push(b'}');
    Ok(())
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
    let trace_id_128 = (u128::from(link.trace_id_high) << 64) | u128::from(link.trace_id);
    map.serialize_entry("trace_id", &fixed_hex::<32>(trace_id_128))?;
    map.serialize_entry("span_id", &fixed_hex::<16>(u128::from(link.span_id)))?;
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
