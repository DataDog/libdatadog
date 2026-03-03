// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Manual protobuf encoding of v04 spans into OTLP TracesData messages.
//!
//! Field number references:
//! - common.proto: KeyValue(1=key, 2=value), AnyValue(1=string, 2=bool, 3=int64, 4=double, 5=array, 7=bytes), ArrayValue(1=values)
//! - resource.proto: Resource(1=attributes)
//! - trace.proto:
//!   - TracesData(1=resource_spans)
//!   - ResourceSpans(1=resource, 2=scope_spans)
//!   - ScopeSpans(2=spans)
//!   - Span(1=trace_id, 2=span_id, 4=parent_span_id, 5=name, 7=start_time_unix_nano,
//!          8=end_time_unix_nano, 9=attributes, 11=events, 13=links, 15=status)
//!   - Status(3=code)
//!   - Span.Event(1=time_unix_nano, 2=name, 3=attributes)
//!   - Span.Link(1=trace_id, 2=span_id, 3=trace_state, 4=attributes, 6=flags)

use libdd_proto_codec::encoder::{BufMut, Encoder, TopLevelEncoder};
use std::borrow::Borrow;

use crate::span::trace_utils::has_top_level;
use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
use crate::span::TraceData;

// common.proto: KeyValue
const KV_KEY: u32 = 1;
const KV_VALUE: u32 = 2;

// common.proto: AnyValue oneof field numbers
const ANY_VALUE_STRING: u32 = 1;
const ANY_VALUE_BOOL: u32 = 2;
const ANY_VALUE_INT: u32 = 3;
const ANY_VALUE_DOUBLE: u32 = 4;
const ANY_VALUE_ARRAY: u32 = 5;
const ANY_VALUE_BYTES: u32 = 7;

// common.proto: ArrayValue
const ARRAY_VALUES: u32 = 1;

// resource.proto: Resource
const RESOURCE_ATTRIBUTES: u32 = 1;

// trace.proto: TracesData
const TRACES_DATA_RESOURCE_SPANS: u32 = 1;

// trace.proto: ResourceSpans
const RESOURCE_SPANS_RESOURCE: u32 = 1;
const RESOURCE_SPANS_SCOPE_SPANS: u32 = 2;

// trace.proto: ScopeSpans
const SCOPE_SPANS_SPANS: u32 = 2;

// trace.proto: Span
const SPAN_TRACE_ID: u32 = 1;
const SPAN_SPAN_ID: u32 = 2;
const SPAN_PARENT_SPAN_ID: u32 = 4;
const SPAN_NAME: u32 = 5;
const SPAN_START_TIME_UNIX_NANO: u32 = 7;
const SPAN_END_TIME_UNIX_NANO: u32 = 8;
const SPAN_ATTRIBUTES: u32 = 9;
const SPAN_EVENTS: u32 = 11;
const SPAN_LINKS: u32 = 13;
const SPAN_STATUS: u32 = 15;

// trace.proto: Status — field 1 is reserved, code is field 3
const STATUS_CODE: u32 = 3;

// trace.proto: Span.Event
const EVENT_TIME_UNIX_NANO: u32 = 1;
const EVENT_NAME: u32 = 2;
const EVENT_ATTRIBUTES: u32 = 3;

// trace.proto: Span.Link
const LINK_TRACE_ID: u32 = 1;
const LINK_SPAN_ID: u32 = 2;
const LINK_TRACE_STATE: u32 = 3;
const LINK_ATTRIBUTES: u32 = 4;
const LINK_FLAGS: u32 = 6;

fn encode_kv_string<B: BufMut>(e: &mut Encoder<'_, B>, kv_field: u32, key: &str, value: &str) {
    let mut kv = e.write_message_repeated(kv_field);
    let mut kv_e = kv.encoder();
    kv_e.write_string_opt(KV_KEY, key);
    kv_e.write_message_repeated(KV_VALUE)
        .encoder()
        .write_string_opt(ANY_VALUE_STRING, value);
}

fn encode_kv_double<B: BufMut>(e: &mut Encoder<'_, B>, kv_field: u32, key: &str, value: f64) {
    let mut kv = e.write_message_repeated(kv_field);
    let mut kv_e = kv.encoder();
    kv_e.write_string_opt(KV_KEY, key);
    kv_e.write_message_repeated(KV_VALUE)
        .encoder()
        .write_f64_opt(ANY_VALUE_DOUBLE, value);
}

fn encode_kv_bytes<B: BufMut>(e: &mut Encoder<'_, B>, kv_field: u32, key: &str, value: &[u8]) {
    let mut kv = e.write_message_repeated(kv_field);
    let mut kv_e = kv.encoder();
    kv_e.write_string(KV_KEY, key);
    kv_e.write_message(KV_VALUE)
        .encoder()
        .write_bytes_opt(ANY_VALUE_BYTES, value);
}

fn encode_attribute_array_value_fields<B: BufMut, T: TraceData>(
    e: &mut Encoder<'_, B>,
    value: &AttributeArrayValue<T>,
) {
    match value {
        AttributeArrayValue::String(s) => e.write_string_opt(ANY_VALUE_STRING, s.borrow()),
        AttributeArrayValue::Boolean(b) => e.write_bool_opt(ANY_VALUE_BOOL, *b),
        AttributeArrayValue::Integer(i) => e.write_int64_opt(ANY_VALUE_INT, *i),
        AttributeArrayValue::Double(d) => e.write_f64_opt(ANY_VALUE_DOUBLE, *d),
    }
}

fn encode_kv_attribute_any_value<B: BufMut, T: TraceData>(
    e: &mut Encoder<'_, B>,
    kv_field: u32,
    key: &str,
    value: &AttributeAnyValue<T>,
) {
    let mut kv = e.write_message_repeated(kv_field);
    let mut kv_e = kv.encoder();
    kv_e.write_string_opt(KV_KEY, key);
    {
        let mut av = kv_e.write_message_repeated(KV_VALUE);
        let mut av_e = av.encoder();
        match value {
            AttributeAnyValue::SingleValue(v) => {
                encode_attribute_array_value_fields(&mut av_e, v);
            }
            AttributeAnyValue::Array(arr) => {
                let mut arr_msg = av_e.write_message_repeated(ANY_VALUE_ARRAY);
                let mut arr_e = arr_msg.encoder();
                for item in arr {
                    let mut item_msg = arr_e.write_message_repeated(ARRAY_VALUES);
                    encode_attribute_array_value_fields(&mut item_msg.encoder(), item);
                }
            }
        }
    }
}

fn encode_span_event<B: BufMut, T: TraceData>(e: &mut Encoder<'_, B>, event: &SpanEvent<T>) {
    let mut ev = e.write_message_repeated(SPAN_EVENTS);
    let mut ev_e = ev.encoder();
    ev_e.write_fixed64_opt(EVENT_TIME_UNIX_NANO, event.time_unix_nano);
    ev_e.write_string_opt(EVENT_NAME, event.name.borrow());
    for (k, v) in &event.attributes {
        encode_kv_attribute_any_value(&mut ev_e, EVENT_ATTRIBUTES, k.borrow(), v);
    }
}

fn encode_span_link<B: BufMut, T: TraceData>(e: &mut Encoder<'_, B>, link: &SpanLink<T>) {
    let mut lk = e.write_message_repeated(SPAN_LINKS);
    let mut lk_e = lk.encoder();

    // trace_id: 16 bytes big-endian — high 64 bits followed by low 64 bits
    let mut trace_id_bytes = [0u8; 16];
    trace_id_bytes[0..8].copy_from_slice(&link.trace_id_high.to_be_bytes());
    trace_id_bytes[8..16].copy_from_slice(&link.trace_id.to_be_bytes());
    lk_e.write_bytes_opt(LINK_TRACE_ID, &trace_id_bytes);

    lk_e.write_bytes_opt(LINK_SPAN_ID, &link.span_id.to_be_bytes());

    if !link.tracestate.borrow().is_empty() {
        lk_e.write_string_opt(LINK_TRACE_STATE, link.tracestate.borrow());
    }

    for (k, v) in &link.attributes {
        encode_kv_string(&mut lk_e, LINK_ATTRIBUTES, k.borrow(), v.borrow());
    }

    if link.flags != 0 {
        lk_e.write_fixed32_opt(LINK_FLAGS, link.flags);
    }
}

/// Encodes a v04 `Span` as an OTLP Span protobuf message into the provided encoder.
///
/// Field mapping:
/// - `trace_id` → `Span.trace_id` (16 bytes big-endian; lower 64 bits of the u128)
/// - `span_id` → `Span.span_id` (8 bytes big-endian)
/// - `parent_id` → `Span.parent_span_id` (8 bytes big-endian; omitted if zero)
/// - `name` → `Span.name`
/// - `start` → `Span.start_time_unix_nano`
/// - `start + duration` → `Span.end_time_unix_nano`
/// - `meta` → `Span.attributes` (string AnyValue)
/// - `metrics` → `Span.attributes` (double AnyValue)
/// - `meta_struct` → `Span.attributes` (bytes AnyValue)
/// - `type` → `Span.attributes["dd.span.type"]` (if non-empty)
/// - `resource` → `Span.attributes["dd.span.resource"]` (if non-empty)
/// - `error != 0` → `Span.status.code = STATUS_CODE_ERROR (2)`
/// - `span_events` → `Span.events`
/// - `span_links` → `Span.links`
///
/// The `service` field is not written here; see [`encode_traces_data`] for the full
/// TracesData wrapper which places `service` in `Resource.attributes["service.name"]`.
fn encode_span<B: BufMut, T: TraceData>(e: &mut Encoder<'_, B>, span: &Span<T>) {
    e.write_bytes_opt(SPAN_TRACE_ID, &span.trace_id.to_be_bytes());
    e.write_bytes_opt(SPAN_SPAN_ID, &span.span_id.to_be_bytes());

    if span.parent_id != 0 {
        e.write_bytes_opt(SPAN_PARENT_SPAN_ID, &span.parent_id.to_be_bytes());
    }

    e.write_string_opt(SPAN_NAME, span.name.borrow());
    e.write_fixed64_opt(SPAN_START_TIME_UNIX_NANO, span.start as u64);
    e.write_fixed64_opt(SPAN_END_TIME_UNIX_NANO, (span.start + span.duration) as u64);

    encode_kv_string(e, SPAN_ATTRIBUTES, "service.name", span.name.borrow());

    for (k, v) in &span.meta {
        encode_kv_string(e, SPAN_ATTRIBUTES, k.borrow(), v.borrow());
    }
    for (k, v) in &span.metrics {
        encode_kv_double(e, SPAN_ATTRIBUTES, k.borrow(), *v);
    }
    for (k, v) in &span.meta_struct {
        encode_kv_bytes(e, SPAN_ATTRIBUTES, k.borrow(), v.borrow());
    }
    if !span.r#type.borrow().is_empty() {
        encode_kv_string(e, SPAN_ATTRIBUTES, "dd.span.type", span.r#type.borrow());
    }
    if !span.resource.borrow().is_empty() {
        encode_kv_string(
            e,
            SPAN_ATTRIBUTES,
            "dd.span.resource",
            span.resource.borrow(),
        );
    }

    for event in &span.span_events {
        encode_span_event(e, event);
    }
    for link in &span.span_links {
        encode_span_link(e, link);
    }

    if span.error != 0 {
        // STATUS_CODE_ERROR = 2
        let mut status = e.write_message(SPAN_STATUS);
        status.encoder().write_int32_opt(STATUS_CODE, 2);
    }
}

/// Encodes a single trace chunk as an OTLP `ResourceSpans` message into the provided encoder.
///
/// The service name is taken from the first span in the chunk where [`has_top_level`] is true.
/// If no top-level span is found the `Resource` message (and its `service.name` attribute)
/// is omitted.  All spans in the chunk are written into a single `ScopeSpans`.
fn encode_resource_spans<B: BufMut, T: TraceData>(e: &mut Encoder<'_, B>, chunk: &[Span<T>]) {
    let mut resource_spans = e.write_message_repeated(TRACES_DATA_RESOURCE_SPANS);
    let mut rs_e = resource_spans.encoder();

    {
        // Resource message is omitted when no service can be determined
        // (write_message skips the field when nothing is written inside it).
        let service = chunk
            .iter()
            .find(|s| has_top_level(s))
            .map(|s| s.service.borrow());
        let mut resource = rs_e.write_message(RESOURCE_SPANS_RESOURCE);
        if let Some(svc) = service {
            if !svc.is_empty() {
                encode_kv_string(
                    &mut resource.encoder(),
                    RESOURCE_ATTRIBUTES,
                    "service.name",
                    svc,
                );
            }
        }
    }

    {
        let mut scope_spans = rs_e.write_message_repeated(RESOURCE_SPANS_SCOPE_SPANS);
        let mut ss_e = scope_spans.encoder();
        for span in chunk {
            let mut span_msg = ss_e.write_message_repeated(SCOPE_SPANS_SPANS);
            encode_span(&mut span_msg.encoder(), span);
        }
    }
}

/// Encodes a list of trace chunks into the OTLP `TracesData` protobuf structure.
///
/// Each chunk becomes one `ResourceSpans`.  The service name for each `ResourceSpans` is
/// determined by finding the first span in the chunk where [`has_top_level`] is true.
///
/// ```text
/// TracesData
///   ResourceSpans          ← one per chunk
///     Resource
///       attributes: [{ key: "service.name", value: <top-level span service> }]
///     ScopeSpans
///       Span …             ← one per span in the chunk
/// ```
///
/// See [`encode_span`] for how individual span fields are mapped.
fn encode_traces_data<B: BufMut, T: TraceData>(
    e: &mut Encoder<'_, B>,
    trace_chunks: &[Vec<Span<T>>],
) {
    for chunk in trace_chunks {
        encode_resource_spans(e, chunk);
    }
}

/// Encodes a list of trace chunks into the OTLP `TracesData` protobuf format and returns
/// the bytes.
///
/// See [`encode_traces_data`] for the structure and field mapping.
pub fn to_buf<T: TraceData, B: BufMut>(trace_chunks: &[Vec<Span<T>>]) -> B {
    let mut enc = TopLevelEncoder::with_capacity(256);
    encode_traces_data(&mut enc.encoder(), trace_chunks);
    enc.finish()
}

/// Encodes a list of trace chunks into the OTLP `TracesData` protobuf format and returns
/// the bytes as a Vec<u8>.
///
/// See [`encode_traces_data`] for the structure and field mapping.
pub fn to_vec<T: TraceData>(trace_chunks: &[Vec<Span<T>>]) -> Vec<u8> {
    to_buf(trace_chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::{
        AttributeAnyValue, AttributeArrayValue, SpanEvent, SpanLink, SpanSlice,
    };
    use prost::Message;
    use std::collections::HashMap;

    // Minimal inline prost structs for round-trip verification.
    // Defined here to avoid a prost build-script dependency on the OTLP .proto files.

    #[derive(prost::Message)]
    struct TracesData {
        #[prost(message, repeated, tag = "1")]
        resource_spans: Vec<ResourceSpans>,
    }

    #[derive(prost::Message)]
    struct ResourceSpans {
        #[prost(message, optional, tag = "1")]
        resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        scope_spans: Vec<ScopeSpans>,
    }

    #[derive(prost::Message)]
    struct Resource {
        #[prost(message, repeated, tag = "1")]
        attributes: Vec<KeyValue>,
    }

    #[derive(prost::Message)]
    struct ScopeSpans {
        #[prost(message, repeated, tag = "2")]
        spans: Vec<OtlpSpan>,
    }

    #[derive(prost::Message, PartialEq)]
    struct OtlpSpan {
        #[prost(bytes = "vec", tag = "1")]
        trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        span_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "4")]
        parent_span_id: Vec<u8>,
        #[prost(string, tag = "5")]
        name: String,
        #[prost(fixed64, tag = "7")]
        start_time_unix_nano: u64,
        #[prost(fixed64, tag = "8")]
        end_time_unix_nano: u64,
        #[prost(message, repeated, tag = "9")]
        attributes: Vec<KeyValue>,
        #[prost(message, repeated, tag = "11")]
        events: Vec<OtlpEvent>,
        #[prost(message, repeated, tag = "13")]
        links: Vec<OtlpLink>,
        #[prost(message, optional, tag = "15")]
        status: Option<OtlpStatus>,
    }

    // AnyValue without the oneof wrapper — protobuf wire format is identical,
    // prost will populate whichever optional field matches the tag.
    #[derive(prost::Message, PartialEq, Clone)]
    struct AnyValue {
        #[prost(string, optional, tag = "1")]
        string_value: Option<String>,
        #[prost(bool, optional, tag = "2")]
        bool_value: Option<bool>,
        #[prost(int64, optional, tag = "3")]
        int_value: Option<i64>,
        #[prost(double, optional, tag = "4")]
        double_value: Option<f64>,
        #[prost(message, optional, tag = "5")]
        array_value: Option<ArrayValue>,
        #[prost(bytes = "vec", optional, tag = "7")]
        bytes_value: Option<Vec<u8>>,
    }

    #[derive(prost::Message, PartialEq, Clone)]
    struct ArrayValue {
        #[prost(message, repeated, tag = "1")]
        values: Vec<AnyValue>,
    }

    #[derive(prost::Message, PartialEq)]
    struct KeyValue {
        #[prost(string, tag = "1")]
        key: String,
        #[prost(message, optional, tag = "2")]
        value: Option<AnyValue>,
    }

    #[derive(prost::Message, PartialEq)]
    struct OtlpEvent {
        #[prost(fixed64, tag = "1")]
        time_unix_nano: u64,
        #[prost(string, tag = "2")]
        name: String,
        #[prost(message, repeated, tag = "3")]
        attributes: Vec<KeyValue>,
    }

    #[derive(prost::Message, PartialEq)]
    struct OtlpLink {
        #[prost(bytes = "vec", tag = "1")]
        trace_id: Vec<u8>,
        #[prost(bytes = "vec", tag = "2")]
        span_id: Vec<u8>,
        #[prost(string, tag = "3")]
        trace_state: String,
        #[prost(message, repeated, tag = "4")]
        attributes: Vec<KeyValue>,
        #[prost(fixed32, tag = "6")]
        flags: u32,
    }

    #[derive(prost::Message, PartialEq)]
    struct OtlpStatus {
        #[prost(int32, tag = "3")]
        code: i32,
    }

    fn decode(bytes: &[u8]) -> TracesData {
        TracesData::decode(bytes).expect("valid protobuf")
    }

    fn find_kv<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a AnyValue> {
        attrs
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.as_ref())
    }

    // Returns a metrics map with `_top_level` set, making `has_top_level` return true.
    fn top_level_metrics() -> HashMap<&'static str, f64> {
        HashMap::from([("_top_level", 1.0)])
    }

    #[test]
    fn test_basic_span_fields() {
        let span = SpanSlice {
            trace_id: 0x0102030405060708090a0b0c0d0e0f10_u128,
            span_id: 0xaabbccdd11223344_u64,
            parent_id: 0x1111222233334444_u64,
            name: "my.operation",
            start: 1_000_000_000,
            duration: 500_000_000,
            ..Default::default()
        };

        let decoded = decode(&to_vec(&[vec![span]]));

        assert_eq!(decoded.resource_spans.len(), 1);
        let rs = &decoded.resource_spans[0];
        // No top-level span with service → resource is omitted
        assert!(rs.resource.is_none());

        assert_eq!(rs.scope_spans.len(), 1);
        let spans = &rs.scope_spans[0].spans;
        assert_eq!(spans.len(), 1);

        let s = &spans[0];
        assert_eq!(
            s.trace_id,
            0x0102030405060708090a0b0c0d0e0f10_u128
                .to_be_bytes()
                .to_vec()
        );
        assert_eq!(s.span_id, 0xaabbccdd11223344_u64.to_be_bytes().to_vec());
        assert_eq!(
            s.parent_span_id,
            0x1111222233334444_u64.to_be_bytes().to_vec()
        );
        assert_eq!(s.name, "my.operation");
        assert_eq!(s.start_time_unix_nano, 1_000_000_000_u64);
        assert_eq!(s.end_time_unix_nano, 1_500_000_000_u64);
        assert!(s.status.is_none());
    }

    #[test]
    fn test_service_taken_from_top_level_span() {
        // Non-top-level span first, then top-level span — service comes from the top-level one.
        let chunk = vec![
            SpanSlice {
                service: "wrong_service",
                name: "child",
                ..Default::default()
            },
            SpanSlice {
                service: "correct_service",
                name: "root",
                metrics: top_level_metrics(),
                ..Default::default()
            },
        ];

        let decoded = decode(&to_vec(&[chunk]));
        let resource = decoded.resource_spans[0]
            .resource
            .as_ref()
            .expect("resource present");
        let svc = find_kv(&resource.attributes, "service.name")
            .expect("service.name attribute")
            .string_value
            .as_deref()
            .unwrap();
        assert_eq!(svc, "correct_service");
    }

    #[test]
    fn test_no_top_level_span_omits_resource() {
        // None of the spans has has_top_level → Resource message must be omitted.
        let chunk = vec![SpanSlice {
            service: "some_service",
            name: "op",
            ..Default::default()
        }];

        let decoded = decode(&to_vec(&[chunk]));
        assert!(decoded.resource_spans[0].resource.is_none());
    }

    #[test]
    fn test_multiple_chunks_produce_multiple_resource_spans() {
        let chunk_a = vec![
            SpanSlice {
                service: "svc_a",
                name: "root_a",
                span_id: 1,
                metrics: top_level_metrics(),
                ..Default::default()
            },
            SpanSlice {
                service: "svc_a",
                name: "child_a",
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_b = vec![SpanSlice {
            service: "svc_b",
            name: "root_b",
            span_id: 3,
            metrics: top_level_metrics(),
            ..Default::default()
        }];

        let decoded = decode(&to_vec(&[chunk_a, chunk_b]));

        assert_eq!(decoded.resource_spans.len(), 2);

        // chunk_a → first ResourceSpans with 2 spans
        let rs_a = &decoded.resource_spans[0];
        let svc_a = find_kv(&rs_a.resource.as_ref().unwrap().attributes, "service.name")
            .unwrap()
            .string_value
            .as_deref()
            .unwrap();
        assert_eq!(svc_a, "svc_a");
        assert_eq!(rs_a.scope_spans[0].spans.len(), 2);

        // chunk_b → second ResourceSpans with 1 span
        let rs_b = &decoded.resource_spans[1];
        let svc_b = find_kv(&rs_b.resource.as_ref().unwrap().attributes, "service.name")
            .unwrap()
            .string_value
            .as_deref()
            .unwrap();
        assert_eq!(svc_b, "svc_b");
        assert_eq!(rs_b.scope_spans[0].spans.len(), 1);
    }

    #[test]
    fn test_span_attributes_from_meta_and_metrics() {
        let span = SpanSlice {
            name: "op",
            r#type: "web",
            resource: "GET /users",
            meta: HashMap::from([("http.method", "GET"), ("env", "prod")]),
            metrics: HashMap::from([("duration_ms", 42.5_f64)]),
            ..Default::default()
        };

        let decoded = decode(&to_vec(&[vec![span]]));
        let attrs = &decoded.resource_spans[0].scope_spans[0].spans[0].attributes;

        assert_eq!(
            find_kv(attrs, "http.method")
                .unwrap()
                .string_value
                .as_deref(),
            Some("GET")
        );
        assert_eq!(
            find_kv(attrs, "env").unwrap().string_value.as_deref(),
            Some("prod")
        );
        assert_eq!(
            find_kv(attrs, "duration_ms").unwrap().double_value,
            Some(42.5)
        );
        assert_eq!(
            find_kv(attrs, "dd.span.type")
                .unwrap()
                .string_value
                .as_deref(),
            Some("web")
        );
        assert_eq!(
            find_kv(attrs, "dd.span.resource")
                .unwrap()
                .string_value
                .as_deref(),
            Some("GET /users")
        );
    }

    #[test]
    fn test_error_maps_to_status_code_error() {
        let span = SpanSlice {
            name: "op",
            error: 1,
            ..Default::default()
        };

        let decoded = decode(&to_vec(&[vec![span]]));
        let status = decoded.resource_spans[0].scope_spans[0].spans[0]
            .status
            .as_ref()
            .expect("status present");
        // STATUS_CODE_ERROR = 2
        assert_eq!(status.code, 2);
    }

    #[test]
    fn test_span_events_encoding() {
        let span = SpanSlice {
            name: "op",
            span_events: vec![SpanEvent {
                time_unix_nano: 9_999_999_999,
                name: "exception",
                attributes: HashMap::from([
                    (
                        "exception.message",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::String(
                            "divide by zero",
                        )),
                    ),
                    (
                        "exception.escaped",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Boolean(true)),
                    ),
                    (
                        "exception.count",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(3)),
                    ),
                    (
                        "exception.duration",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Double(0.5)),
                    ),
                    (
                        "exception.lines",
                        AttributeAnyValue::Array(vec![
                            AttributeArrayValue::String("line 1"),
                            AttributeArrayValue::String("line 2"),
                        ]),
                    ),
                ]),
            }],
            ..Default::default()
        };

        let decoded = decode(&to_vec(&[vec![span]]));
        let events = &decoded.resource_spans[0].scope_spans[0].spans[0].events;
        assert_eq!(events.len(), 1);

        let ev = &events[0];
        assert_eq!(ev.time_unix_nano, 9_999_999_999);
        assert_eq!(ev.name, "exception");

        assert_eq!(
            find_kv(&ev.attributes, "exception.message")
                .unwrap()
                .string_value
                .as_deref(),
            Some("divide by zero")
        );
        assert_eq!(
            find_kv(&ev.attributes, "exception.escaped")
                .unwrap()
                .bool_value,
            Some(true)
        );
        assert_eq!(
            find_kv(&ev.attributes, "exception.count")
                .unwrap()
                .int_value,
            Some(3)
        );
        assert_eq!(
            find_kv(&ev.attributes, "exception.duration")
                .unwrap()
                .double_value,
            Some(0.5)
        );

        let array_val = find_kv(&ev.attributes, "exception.lines")
            .unwrap()
            .array_value
            .as_ref()
            .expect("array value");
        assert_eq!(array_val.values.len(), 2);
        assert_eq!(array_val.values[0].string_value.as_deref(), Some("line 1"));
        assert_eq!(array_val.values[1].string_value.as_deref(), Some("line 2"));
    }

    #[test]
    fn test_span_links_encoding() {
        let span = SpanSlice {
            name: "op",
            span_links: vec![SpanLink {
                trace_id: 0x0102030405060708,
                trace_id_high: 0x090a0b0c0d0e0f10,
                span_id: 0xaabbccdd11223344,
                tracestate: "vendor=value",
                flags: 0x00000300,
                attributes: HashMap::from([("link.attr", "value")]),
            }],
            ..Default::default()
        };

        let decoded = decode(&to_vec(&[vec![span]]));
        let links = &decoded.resource_spans[0].scope_spans[0].spans[0].links;
        assert_eq!(links.len(), 1);

        let lk = &links[0];

        let mut expected_trace_id = [0u8; 16];
        expected_trace_id[0..8].copy_from_slice(&0x090a0b0c0d0e0f10_u64.to_be_bytes());
        expected_trace_id[8..16].copy_from_slice(&0x0102030405060708_u64.to_be_bytes());
        assert_eq!(lk.trace_id, expected_trace_id.to_vec());

        assert_eq!(lk.span_id, 0xaabbccdd11223344_u64.to_be_bytes().to_vec());
        assert_eq!(lk.trace_state, "vendor=value");
        assert_eq!(lk.flags, 0x00000300);
        assert_eq!(
            find_kv(&lk.attributes, "link.attr")
                .unwrap()
                .string_value
                .as_deref(),
            Some("value")
        );
    }
}
