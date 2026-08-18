// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM JSON encoder.
//!
//! Encodes Datadog v04 trace chunks or normalized protobuf spans to the JSON body accepted by the
//! Datadog HTTP trace intake (`POST /v1/input`).
//!
//! ## Differences from the regular agent (msgpack v04) encoding
//!
//! - **Wire format**: JSON, wrapped as `{"traces": [ {hostname, env, ..., spans: [...] }, ... ]}`.
//!   Per-trace metadata (hostname, env, language*, tracerVersion, runtimeID, containerID) is
//!   inlined on each trace instead of being carried in request headers. Hostname is always emitted
//! - **IDs**: `trace_id`, `span_id`, `parent_id` are lowercase hex strings (16 chars; 32 for
//!   span-link trace IDs)
//! - **Time**: span `start` is whole seconds since Unix epoch; `duration` remains nanoseconds
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
//! The trace exporter normalizes and obfuscates spans before calling this encoder.

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
use crate::span::{TraceData, SPAN_LINK_FLAGS_SET_SENTINEL};
use crate::tracer_metadata::TracerMetadata;
use libdd_trace_protobuf::pb;
use serde::{
    ser::{Error as _, SerializeMap, SerializeSeq},
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

trait AgentlessSpan {
    type Event: AgentlessSpanEvent;
    type Link: AgentlessSpanLink;

    fn service(&self) -> &str;
    fn name(&self) -> &str;
    fn resource(&self) -> &str;
    fn span_type(&self) -> &str;
    fn trace_id(&self) -> u64;
    fn trace_id_high(&self) -> u64;
    fn span_id(&self) -> u64;
    fn parent_id(&self) -> u64;
    fn start(&self) -> i64;
    fn duration(&self) -> i64;
    fn error(&self) -> i32;
    fn meta(&self) -> impl Iterator<Item = (&str, &str)>;
    fn metrics(&self) -> impl Iterator<Item = (&str, f64)>;
    fn meta_struct(&self) -> impl Iterator<Item = (&str, &[u8])>;
    fn meta_struct_is_empty(&self) -> bool;
    fn span_links(&self) -> &[Self::Link];
    fn span_events(&self) -> &[Self::Event];
}

impl<T: TraceData> AgentlessSpan for Span<T> {
    type Event = SpanEvent<T>;
    type Link = SpanLink<T>;

    fn service(&self) -> &str {
        self.service.borrow()
    }

    fn name(&self) -> &str {
        self.name.borrow()
    }

    fn resource(&self) -> &str {
        self.resource.borrow()
    }

    fn span_type(&self) -> &str {
        self.r#type.borrow()
    }

    fn trace_id(&self) -> u64 {
        self.trace_id as u64
    }

    fn trace_id_high(&self) -> u64 {
        (self.trace_id >> 64) as u64
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn parent_id(&self) -> u64 {
        self.parent_id
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn error(&self) -> i32 {
        self.error
    }

    fn meta(&self) -> impl Iterator<Item = (&str, &str)> {
        self.meta
            .iter()
            .map(|(key, value)| (key.borrow(), value.borrow()))
    }

    fn metrics(&self) -> impl Iterator<Item = (&str, f64)> {
        self.metrics
            .iter()
            .map(|(key, value)| (key.borrow(), *value))
    }

    fn meta_struct(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.meta_struct
            .iter()
            .map(|(key, value)| (key.borrow(), value.borrow()))
    }

    fn meta_struct_is_empty(&self) -> bool {
        self.meta_struct.is_empty()
    }

    fn span_links(&self) -> &[Self::Link] {
        &self.span_links
    }

    fn span_events(&self) -> &[Self::Event] {
        &self.span_events
    }
}

impl AgentlessSpan for pb::Span {
    type Event = pb::SpanEvent;
    type Link = pb::SpanLink;

    fn service(&self) -> &str {
        &self.service
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn resource(&self) -> &str {
        &self.resource
    }

    fn span_type(&self) -> &str {
        &self.r#type
    }

    fn trace_id(&self) -> u64 {
        self.trace_id
    }

    fn trace_id_high(&self) -> u64 {
        0
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn parent_id(&self) -> u64 {
        self.parent_id
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn error(&self) -> i32 {
        self.error
    }

    fn meta(&self) -> impl Iterator<Item = (&str, &str)> {
        self.meta
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    fn metrics(&self) -> impl Iterator<Item = (&str, f64)> {
        self.metrics
            .iter()
            .map(|(key, value)| (key.as_str(), *value))
    }

    fn meta_struct(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.meta_struct
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_slice()))
    }

    fn meta_struct_is_empty(&self) -> bool {
        self.meta_struct.is_empty()
    }

    fn span_links(&self) -> &[Self::Link] {
        &self.span_links
    }

    fn span_events(&self) -> &[Self::Event] {
        &self.span_events
    }
}

trait AgentlessSpanLink {
    fn trace_id(&self) -> u64;
    fn trace_id_high(&self) -> u64;
    fn span_id(&self) -> u64;
    fn attributes(&self) -> impl Iterator<Item = (&str, &str)>;
    fn attributes_len(&self) -> usize;
    fn tracestate(&self) -> &str;
    fn flags(&self) -> u32;
}

impl<T: TraceData> AgentlessSpanLink for SpanLink<T> {
    fn trace_id(&self) -> u64 {
        self.trace_id
    }

    fn trace_id_high(&self) -> u64 {
        self.trace_id_high
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.attributes
            .iter()
            .map(|(key, value)| (key.borrow(), value.borrow()))
    }

    fn attributes_len(&self) -> usize {
        self.attributes.len()
    }

    fn tracestate(&self) -> &str {
        self.tracestate.borrow()
    }

    fn flags(&self) -> u32 {
        self.flags
    }
}

impl AgentlessSpanLink for pb::SpanLink {
    fn trace_id(&self) -> u64 {
        self.trace_id
    }

    fn trace_id_high(&self) -> u64 {
        self.trace_id_high
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.attributes
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    fn attributes_len(&self) -> usize {
        self.attributes.len()
    }

    fn tracestate(&self) -> &str {
        &self.tracestate
    }

    fn flags(&self) -> u32 {
        self.flags
    }
}

trait AgentlessSpanEvent {
    type Attribute: AgentlessAttribute;

    fn time_unix_nano(&self) -> u64;
    fn name(&self) -> &str;
    fn attributes(&self) -> impl Iterator<Item = (&str, &Self::Attribute)>;
    fn attributes_len(&self) -> usize;
}

impl<T: TraceData> AgentlessSpanEvent for SpanEvent<T> {
    type Attribute = AttributeAnyValue<T>;

    fn time_unix_nano(&self) -> u64 {
        self.time_unix_nano
    }

    fn name(&self) -> &str {
        self.name.borrow()
    }

    fn attributes(&self) -> impl Iterator<Item = (&str, &Self::Attribute)> {
        self.attributes
            .iter()
            .map(|(key, value)| (key.borrow(), value))
    }

    fn attributes_len(&self) -> usize {
        self.attributes.len()
    }
}

impl AgentlessSpanEvent for pb::SpanEvent {
    type Attribute = pb::AttributeAnyValue;

    fn time_unix_nano(&self) -> u64 {
        self.time_unix_nano
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn attributes(&self) -> impl Iterator<Item = (&str, &Self::Attribute)> {
        self.attributes
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    fn attributes_len(&self) -> usize {
        self.attributes.len()
    }
}

trait AgentlessAttribute {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
}

/// Encode the given `traces` to the agentless JSON payload (`/v1/input` body).
///
/// Returns the serialized JSON bytes on success.
pub fn encode_payload<T: TraceData>(
    traces: &[Vec<Span<T>>],
    metadata: &TracerMetadata,
) -> Result<Vec<u8>, serde_json::Error> {
    encode_payload_inner(traces, metadata)
}

/// Encode normalized protobuf spans to the agentless JSON payload (`/v1/input` body).
///
/// Returns the serialized JSON bytes on success.
pub fn encode_protobuf_payload(
    traces: &[Vec<pb::Span>],
    metadata: &TracerMetadata,
) -> Result<Vec<u8>, serde_json::Error> {
    encode_payload_inner(traces, metadata)
}

fn encode_payload_inner<T: AgentlessSpan>(
    traces: &[Vec<T>],
    metadata: &TracerMetadata,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut bytes);

    let mut map_ser = serializer.serialize_map(Some(1))?;
    map_ser.serialize_entry(
        "traces",
        &ser_fn!(<T: AgentlessSpan> |ser, traces: &'a [Vec<T>], metadata: &'a TracerMetadata| {
            let mut traces_serializer = ser.serialize_seq(Some(traces.len()))?;
            for chunk in traces {
                traces_serializer.serialize_element(&ser_fn!(<T: AgentlessSpan> |ser, chunk: &'a Vec<T>, metadata: &'a TracerMetadata| {
                    encode_trace(ser, chunk, metadata)
                }))?;
            }
            traces_serializer.end()
        }),
    )?;
    SerializeMap::end(map_ser)?;
    Ok(bytes)
}

fn encode_trace<T: AgentlessSpan, S: Serializer>(
    ser: S,
    chunk: &[T],
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
        &ser_fn!(<T: AgentlessSpan> |ser, chunk: &'a [T]| {
            let mut seq = ser.serialize_seq(Some(chunk.len()))?;
            for (i, span) in chunk.iter().enumerate() {
                let is_first = i == 0;
                seq.serialize_element(&ser_fn!(<T: AgentlessSpan> |ser, span: &'a T, is_first: bool| {
                    encode_span(ser, span, is_first)
                }))?;
            }
            seq.end()
        }),
    )?;

    map.end()
}

fn encode_span<T: AgentlessSpan, S: Serializer>(
    ser: S,
    span: &T,
    is_first_in_trace: bool,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;

    let trace_id = span.trace_id();
    map.serialize_entry(
        "trace_id",
        &ser_fn!(|ser, trace_id: u64| { ser.collect_str(&format_args!("{trace_id:016x}")) }),
    )?;
    let span_id = span.span_id();
    map.serialize_entry(
        "span_id",
        &ser_fn!(|ser, span_id: u64| { ser.collect_str(&format_args!("{span_id:016x}")) }),
    )?;
    let parent_id = span.parent_id();
    map.serialize_entry(
        "parent_id",
        &ser_fn!(|ser, parent_id: u64| { ser.collect_str(&format_args!("{parent_id:016x}")) }),
    )?;

    // Resource defaults to name when empty.
    let name_str = span.name();
    let resource_str = span.resource();
    let service_str = span.service();
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
    map.serialize_entry("error", &span.error())?;
    map.serialize_entry("start", &(span.start().max(0) / 1_000_000_000))?;
    map.serialize_entry("duration", &span.duration())?;

    let type_str = span.span_type();
    if !type_str.is_empty() {
        map.serialize_entry("type", type_str)?;
    }

    map.serialize_entry(
        "meta",
        &ser_fn!(<T: AgentlessSpan> |ser, span: &'a T, is_first_in_trace: bool| {
            let upper_bits = span.trace_id_high();
            let mut p_tid_seen = false;
            let mut span_links_seen = false;
            let mut events_seen = false;
            let mut compute_stats_seen = false;

            let mut meta = ser.serialize_map(None)?;
            for (key, value) in span.meta() {
                match key {
                    "_dd.p.tid" => p_tid_seen = true,
                    "_dd.span_links" => span_links_seen = true,
                    "events" => events_seen = true,
                    "_dd.compute_stats"=> compute_stats_seen = true,
                    _ => {}
                };
                meta.serialize_entry(key, value)?;
            }
            if !p_tid_seen && upper_bits != 0 {
                meta.serialize_entry(
                    "_dd.p.tid",
                    &ser_fn!(|ser, upper_bits: u64| {
                        ser.collect_str(&format_args!("{:016x}", upper_bits as u64))
                    }),
                )?;
            }
            if !span_links_seen && !span.span_links().is_empty() {
                if let Some(s) = serialize_span_links(span.span_links()) {
                    meta.serialize_entry("_dd.span_links", &s)?;
                }
            }
            if !events_seen && !span.span_events().is_empty() {
                if let Some(s) = serialize_span_events(span.span_events()) {
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
        &ser_fn!(<T: AgentlessSpan> |ser, span: &'a T| {
            let mut metrics = ser.serialize_map(None)?;
            let mut trace_root_seen = false;
            for (key, value) in span.metrics() {
                // serde_json refuses to serialize NaN/Inf; drop them silently.
                if value.is_finite() {
                    match key {
                        "_trace_root" => trace_root_seen = true,
                        "_top_level" => {
                            metrics.serialize_entry(key, &(value as u32))?;
                            continue
                        },
                        _ => {},
                    }
                    metrics.serialize_entry(key, &value)?
                }
            }
            if !trace_root_seen && span.parent_id() == 0 {
                metrics.serialize_entry("_trace_root", &1u32)?;
            }
            metrics.end()
        }),
    )?;

    if !span.meta_struct_is_empty() {
        map.serialize_entry(
            "meta_struct",
            &ser_fn!(<T: AgentlessSpan> |ser, span: &'a T| {
                let mut ms = ser.serialize_map(None)?;
                for (key, bytes) in span.meta_struct() {
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
fn serialize_span_links<T: AgentlessSpanLink>(links: &[T]) -> Option<String> {
    let s = serde_json::to_string(&ser_fn!(<T: AgentlessSpanLink> |ser, links: &'a [T]| {
        let mut seq = ser.serialize_seq(Some(links.len()))?;
        for link in links {
            seq.serialize_element(&ser_fn!(<T: AgentlessSpanLink> |ser, link: &'a T| {
                encode_span_link(ser, link)
            }))?;
        }
        seq.end()
    }))
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

fn encode_span_link<T: AgentlessSpanLink, S: Serializer>(
    ser: S,
    link: &T,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    let trace_id_128: u128 = ((link.trace_id_high() as u128) << 64) | (link.trace_id() as u128);
    map.serialize_entry("trace_id", &format!("{:032x}", trace_id_128))?;
    map.serialize_entry("span_id", &format!("{:016x}", link.span_id()))?;
    if link.attributes_len() != 0 {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: AgentlessSpanLink> |ser, link: &'a T| {
                let mut attrs = ser.serialize_map(Some(link.attributes_len()))?;
                for (key, value) in link.attributes() {
                    attrs.serialize_entry(key, value)?;
                }
                attrs.end()
            }),
        )?;
    }
    // When `flags` is 0, no sampling decision exists, so omit the field. Before emission,
    // mask off the internal "explicitly set" sentinel (bit 31), because this JSON field uses
    // the same `_dd.span_links` key that the v0.5 encoder produces and must match its output.
    if link.flags() != 0 {
        map.serialize_entry(
            "flags",
            &((link.flags() & !SPAN_LINK_FLAGS_SET_SENTINEL) as u64),
        )?;
    }
    let tracestate = link.tracestate();
    if !tracestate.is_empty() {
        map.serialize_entry("tracestate", tracestate)?;
    }
    map.end()
}

/// Serialize span events to a JSON string suitable for `meta['events']`.
fn serialize_span_events<T: AgentlessSpanEvent>(events: &[T]) -> Option<String> {
    let s = serde_json::to_string(&ser_fn!(<T: AgentlessSpanEvent> |ser, events: &'a [T]| {
        let mut seq = ser.serialize_seq(Some(events.len()))?;
        for event in events {
            seq.serialize_element(&ser_fn!(<T: AgentlessSpanEvent> |ser, event: &'a T| {
                encode_span_event(ser, event)
            }))?;
        }
        seq.end()
    }))
    .ok()?;
    Some(truncate_with_ellipsis(s, MAX_META_VALUE_LEN))
}

fn encode_span_event<T: AgentlessSpanEvent, S: Serializer>(
    ser: S,
    event: &T,
) -> Result<S::Ok, S::Error> {
    let mut map = ser.serialize_map(None)?;
    map.serialize_entry("name", event.name())?;
    map.serialize_entry("time_unix_nano", &event.time_unix_nano())?;
    if event.attributes_len() != 0 {
        map.serialize_entry(
            "attributes",
            &ser_fn!(<T: AgentlessSpanEvent> |ser, event: &'a T| {
                let mut attrs = ser.serialize_map(Some(event.attributes_len()))?;
                for (key, value) in event.attributes() {
                    attrs.serialize_entry(key, &ser_fn!(<A: AgentlessAttribute> |ser, value: &'a A| {
                        value.serialize(ser)
                    }))?;
                }
                attrs.end()
            }),
        )?;
    }
    map.end()
}

impl<T: TraceData> AgentlessAttribute for AttributeAnyValue<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AttributeAnyValue::SingleValue(value) => serialize_scalar(serializer, value),
            AttributeAnyValue::Array(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(
                        &ser_fn!(<T: TraceData> |ser, value: &'a AttributeArrayValue<T>| {
                            serialize_scalar(ser, value)
                        }),
                    )?;
                }
                sequence.end()
            }
        }
    }
}

impl AgentlessAttribute for pb::AttributeAnyValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use pb::attribute_any_value::AttributeAnyValueType;

        match self.r#type() {
            AttributeAnyValueType::StringValue => serializer.serialize_str(&self.string_value),
            AttributeAnyValueType::BoolValue => serializer.serialize_bool(self.bool_value),
            AttributeAnyValueType::IntValue => serializer.serialize_i64(self.int_value),
            AttributeAnyValueType::DoubleValue => serialize_f64(serializer, self.double_value),
            AttributeAnyValueType::ArrayValue => {
                let values = self.array_value.as_ref().ok_or_else(|| {
                    S::Error::custom("agentless span event array is missing its values")
                })?;
                let mut sequence = serializer.serialize_seq(Some(values.values.len()))?;
                for value in &values.values {
                    sequence.serialize_element(&ser_fn!(
                        |ser, value: &'a pb::AttributeArrayValue| {
                            serialize_protobuf_scalar(ser, value)
                        }
                    ))?;
                }
                sequence.end()
            }
        }
    }
}

fn serialize_scalar<S: Serializer, T: TraceData>(
    serializer: S,
    value: &AttributeArrayValue<T>,
) -> Result<S::Ok, S::Error> {
    match value {
        AttributeArrayValue::String(value) => {
            let value: &str = value.borrow();
            serializer.serialize_str(value)
        }
        AttributeArrayValue::Boolean(value) => serializer.serialize_bool(*value),
        AttributeArrayValue::Integer(value) => serializer.serialize_i64(*value),
        AttributeArrayValue::Double(value) => serialize_f64(serializer, *value),
    }
}

fn serialize_protobuf_scalar<S: Serializer>(
    serializer: S,
    value: &pb::AttributeArrayValue,
) -> Result<S::Ok, S::Error> {
    use pb::attribute_array_value::AttributeArrayValueType;

    match value.r#type() {
        AttributeArrayValueType::StringValue => serializer.serialize_str(&value.string_value),
        AttributeArrayValueType::BoolValue => serializer.serialize_bool(value.bool_value),
        AttributeArrayValueType::IntValue => serializer.serialize_i64(value.int_value),
        AttributeArrayValueType::DoubleValue => serialize_f64(serializer, value.double_value),
    }
}

fn serialize_f64<S: Serializer>(serializer: S, value: f64) -> Result<S::Ok, S::Error> {
    if value.is_finite() {
        serializer.serialize_f64(value)
    } else {
        // NaN/Inf become JSON null.
        serializer.serialize_unit()
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
