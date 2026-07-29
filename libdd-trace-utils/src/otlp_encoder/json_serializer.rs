// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes the generated prost OTLP types to OTLP-spec HTTP/JSON. Trace/span ids are
//! lowercase hex, 64-bit integers (incl. timestamps) are decimal strings, `bytesValue` is
//! base64, enums are integers, field names are lowerCamelCase, and proto3 defaults are omitted.
//! This is the only place the OTLP/JSON wire shape is defined: the prost types are the single
//! source of truth, serialized directly to the OTLP/JSON wire format here.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::ser::{Serialize, SerializeMap, Serializer};

use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value::Value as ProtoValue, AnyValue, ArrayValue, InstrumentationScope, KeyValue,
    KeyValueList,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
    span::{Event, Link},
    ResourceSpans, ScopeSpans, Span, Status,
};

/// Top-level wrapper: `serde_json::to_vec(&OtlpJson(req))` yields the OTLP/JSON body.
pub(crate) struct OtlpJson<'a>(pub &'a ExportTraceServiceRequest);

pub(crate) fn to_otlp_json_vec(req: &ExportTraceServiceRequest) -> serde_json::Result<Vec<u8>> {
    // Plain `to_vec` is intentional. serde_json sizes no allocation from the `serialize_map`/
    // `serialize_seq` length hints (it only special-cases `Some(0)`), and benchmarking output-
    // buffer pre-sizing showed no win: sizing from `encoded_len()` regressed (~3% — the extra
    // traversal costs more than the saved reallocations), and a cheap span-count estimate was
    // within noise.
    serde_json::to_vec(&OtlpJson(req))
}

/// Serialize a `&[T]` by wrapping each element with `W`.
fn seq<'a, T, W, S>(s: S, items: &'a [T], wrap: fn(&'a T) -> W) -> Result<S::Ok, S::Error>
where
    W: Serialize,
    S: Serializer,
{
    // `collect_seq` forwards the iterator's size hint as the sequence length, so there's no need
    // to go through `serialize_seq` just to supply it.
    s.collect_seq(items.iter().map(wrap))
}

/// Serializes an OTLP id (`trace_id`/`span_id`/`parent_span_id`) as lowercase hex without
/// allocating a `String`. OTLP ids are 8 or 16 bytes (16/32 hex chars); a 64-byte stack buffer
/// covers them, with an allocating `hex::encode` fallback for any unexpectedly-long input.
struct HexId<'a>(&'a [u8]);
impl Serialize for HexId<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut buf = [0u8; 64];
        let n = self.0.len() * 2;
        if n <= buf.len() && hex::encode_to_slice(self.0, &mut buf[..n]).is_ok() {
            if let Ok(hex) = std::str::from_utf8(&buf[..n]) {
                return s.serialize_str(hex);
            }
        }
        s.serialize_str(&hex::encode(self.0))
    }
}

/// Serializes a 64-bit integer as a decimal JSON *string* — OTLP encodes `int64`/`uint64` (incl.
/// nanosecond timestamps) as strings to avoid IEEE-754 precision loss — without allocating.
struct NumStr<T: core::fmt::Display>(T);
impl<T: core::fmt::Display> Serialize for NumStr<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use core::fmt::Write as _;
        // u64::MAX and i64::MIN are at most 20 chars; 24 bytes is ample.
        let mut buf = DecimalBuf::default();
        if write!(buf, "{}", self.0).is_ok() {
            return s.serialize_str(buf.as_str());
        }
        s.serialize_str(&self.0.to_string())
    }
}

/// A fixed-capacity, stack-allocated [`core::fmt::Write`] sink: it formats a value into an inline
/// `[u8; 24]` buffer with no heap allocation. Used by [`NumStr`] to render a 64-bit integer as a
/// decimal string (`u64::MAX` / `i64::MIN` are at most 20 chars, so 24 bytes always suffices).
#[derive(Default)]
struct DecimalBuf {
    buf: [u8; 24],
    len: usize,
}
impl DecimalBuf {
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}
impl core::fmt::Write for DecimalBuf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Serialize for OtlpJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("resourceSpans", &ResourceSpansSeq(&self.0.resource_spans))?;
        m.end()
    }
}

struct ResourceSpansSeq<'a>(&'a [ResourceSpans]);
impl Serialize for ResourceSpansSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, ResourceSpansJson)
    }
}

struct ResourceSpansJson<'a>(&'a ResourceSpans);
impl Serialize for ResourceSpansJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let rs = self.0;
        let mut m = s.serialize_map(None)?;
        if let Some(r) = &rs.resource {
            m.serialize_entry("resource", &ResourceJson(r))?;
        }
        if !rs.scope_spans.is_empty() {
            m.serialize_entry("scopeSpans", &ScopeSpansSeq(&rs.scope_spans))?;
        }
        if !rs.schema_url.is_empty() {
            m.serialize_entry("schemaUrl", &rs.schema_url)?;
        }
        m.end()
    }
}

struct ResourceJson<'a>(&'a Resource);
impl Serialize for ResourceJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let r = self.0;
        let mut m = s.serialize_map(None)?;
        if !r.attributes.is_empty() {
            m.serialize_entry("attributes", &KeyValueSeq(&r.attributes))?;
        }
        if r.dropped_attributes_count != 0 {
            m.serialize_entry("droppedAttributesCount", &r.dropped_attributes_count)?;
        }
        // `entity_refs` is a profiling-signal field, not part of the trace JSON shape — omitted.
        m.end()
    }
}

struct ScopeSpansSeq<'a>(&'a [ScopeSpans]);
impl Serialize for ScopeSpansSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, ScopeSpansJson)
    }
}

struct ScopeSpansJson<'a>(&'a ScopeSpans);
impl Serialize for ScopeSpansJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let ss = self.0;
        let mut m = s.serialize_map(None)?;
        if let Some(scope) = &ss.scope {
            m.serialize_entry("scope", &ScopeJson(scope))?;
        }
        if !ss.spans.is_empty() {
            m.serialize_entry("spans", &SpanSeq(&ss.spans))?;
        }
        if !ss.schema_url.is_empty() {
            m.serialize_entry("schemaUrl", &ss.schema_url)?;
        }
        m.end()
    }
}

struct ScopeJson<'a>(&'a InstrumentationScope);
impl Serialize for ScopeJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sc = self.0;
        let mut m = s.serialize_map(None)?;
        if !sc.name.is_empty() {
            m.serialize_entry("name", &sc.name)?;
        }
        if !sc.version.is_empty() {
            m.serialize_entry("version", &sc.version)?;
        }
        if !sc.attributes.is_empty() {
            m.serialize_entry("attributes", &KeyValueSeq(&sc.attributes))?;
        }
        if sc.dropped_attributes_count != 0 {
            m.serialize_entry("droppedAttributesCount", &sc.dropped_attributes_count)?;
        }
        m.end()
    }
}

struct SpanSeq<'a>(&'a [Span]);
impl Serialize for SpanSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, SpanJson)
    }
}

struct SpanJson<'a>(&'a Span);
impl Serialize for SpanJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let sp = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("traceId", &HexId(&sp.trace_id))?;
        m.serialize_entry("spanId", &HexId(&sp.span_id))?;
        if !sp.parent_span_id.is_empty() {
            m.serialize_entry("parentSpanId", &HexId(&sp.parent_span_id))?;
        }
        if !sp.trace_state.is_empty() {
            m.serialize_entry("traceState", &sp.trace_state)?;
        }
        m.serialize_entry("name", &sp.name)?;
        m.serialize_entry("kind", &sp.kind)?;
        m.serialize_entry("startTimeUnixNano", &NumStr(sp.start_time_unix_nano))?;
        m.serialize_entry("endTimeUnixNano", &NumStr(sp.end_time_unix_nano))?;
        if !sp.attributes.is_empty() {
            m.serialize_entry("attributes", &KeyValueSeq(&sp.attributes))?;
        }
        if sp.dropped_attributes_count != 0 {
            m.serialize_entry("droppedAttributesCount", &sp.dropped_attributes_count)?;
        }
        if !sp.events.is_empty() {
            m.serialize_entry("events", &EventSeq(&sp.events))?;
        }
        if sp.dropped_events_count != 0 {
            m.serialize_entry("droppedEventsCount", &sp.dropped_events_count)?;
        }
        if !sp.links.is_empty() {
            m.serialize_entry("links", &LinkSeq(&sp.links))?;
        }
        if sp.dropped_links_count != 0 {
            m.serialize_entry("droppedLinksCount", &sp.dropped_links_count)?;
        }
        if let Some(st) = &sp.status {
            m.serialize_entry("status", &StatusJson(st))?;
        }
        if sp.flags != 0 {
            m.serialize_entry("flags", &sp.flags)?;
        }
        m.end()
    }
}

struct StatusJson<'a>(&'a Status);
impl Serialize for StatusJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let st = self.0;
        let mut m = s.serialize_map(None)?;
        if !st.message.is_empty() {
            m.serialize_entry("message", &st.message)?;
        }
        m.serialize_entry("code", &st.code)?;
        m.end()
    }
}

struct EventSeq<'a>(&'a [Event]);
impl Serialize for EventSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, EventJson)
    }
}

struct EventJson<'a>(&'a Event);
impl Serialize for EventJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let e = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("timeUnixNano", &NumStr(e.time_unix_nano))?;
        m.serialize_entry("name", &e.name)?;
        if !e.attributes.is_empty() {
            m.serialize_entry("attributes", &KeyValueSeq(&e.attributes))?;
        }
        if e.dropped_attributes_count != 0 {
            m.serialize_entry("droppedAttributesCount", &e.dropped_attributes_count)?;
        }
        m.end()
    }
}

struct LinkSeq<'a>(&'a [Link]);
impl Serialize for LinkSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, LinkJson)
    }
}

struct LinkJson<'a>(&'a Link);
impl Serialize for LinkJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let l = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("traceId", &HexId(&l.trace_id))?;
        m.serialize_entry("spanId", &HexId(&l.span_id))?;
        if !l.trace_state.is_empty() {
            m.serialize_entry("traceState", &l.trace_state)?;
        }
        if !l.attributes.is_empty() {
            m.serialize_entry("attributes", &KeyValueSeq(&l.attributes))?;
        }
        if l.dropped_attributes_count != 0 {
            m.serialize_entry("droppedAttributesCount", &l.dropped_attributes_count)?;
        }
        if l.flags != 0 {
            m.serialize_entry("flags", &l.flags)?;
        }
        m.end()
    }
}

struct KeyValueSeq<'a>(&'a [KeyValue]);
impl Serialize for KeyValueSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, KeyValueJson)
    }
}

struct KeyValueJson<'a>(&'a KeyValue);
impl Serialize for KeyValueJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let kv = self.0;
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("key", &kv.key)?;
        // `key_ref` is a profiling-signal field, not part of the trace JSON shape — omitted.
        if let Some(v) = &kv.value {
            m.serialize_entry("value", &AnyValueJson(v))?;
        }
        m.end()
    }
}

struct AnyValueJson<'a>(&'a AnyValue);
impl Serialize for AnyValueJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match &self.0.value {
            Some(ProtoValue::StringValue(v)) => m.serialize_entry("stringValue", v)?,
            Some(ProtoValue::BoolValue(v)) => m.serialize_entry("boolValue", v)?,
            // int64 must be a string to avoid precision loss in JSON.
            Some(ProtoValue::IntValue(v)) => m.serialize_entry("intValue", &NumStr(*v))?,
            Some(ProtoValue::DoubleValue(v)) => m.serialize_entry("doubleValue", v)?,
            Some(ProtoValue::BytesValue(v)) => {
                m.serialize_entry("bytesValue", &STANDARD.encode(v))?
            }
            Some(ProtoValue::ArrayValue(a)) => {
                m.serialize_entry("arrayValue", &ArrayValueJson(a))?
            }
            Some(ProtoValue::KvlistValue(kv)) => {
                m.serialize_entry("kvlistValue", &KvListJson(kv))?
            }
            Some(ProtoValue::StringValueRef(_)) | None => {}
        }
        m.end()
    }
}

struct ArrayValueJson<'a>(&'a ArrayValue);
impl Serialize for ArrayValueJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("values", &AnyValueSeq(&self.0.values))?;
        m.end()
    }
}

struct KvListJson<'a>(&'a KeyValueList);
impl Serialize for KvListJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("values", &KeyValueSeq(&self.0.values))?;
        m.end()
    }
}

struct AnyValueSeq<'a>(&'a [AnyValue]);
impl Serialize for AnyValueSeq<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        seq(s, self.0, AnyValueJson)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
        any_value::Value as PV, AnyValue, KeyValue,
    };
    use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
        span::Link, ResourceSpans, ScopeSpans, Span, Status,
    };

    fn span_json(s: Span) -> serde_json::Value {
        let req = ProtoReq {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![s],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };
        let bytes = to_otlp_json_vec(&req).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["resourceSpans"][0]
            ["scopeSpans"][0]["spans"][0]
            .clone()
    }

    fn base_span() -> Span {
        Span {
            trace_id: 0x5b8efff798038103_d269b633813fc60c_u128
                .to_be_bytes()
                .to_vec(),
            span_id: 0xEEE19B7EC3C1B174u64.to_be_bytes().to_vec(),
            trace_state: String::new(),
            parent_span_id: Vec::new(),
            flags: 0,
            name: "res".to_string(),
            kind: 2,
            start_time_unix_nano: 1544712660000000000,
            end_time_unix_nano: 1544712661000000000,
            attributes: Vec::new(),
            dropped_attributes_count: 0,
            events: Vec::new(),
            dropped_events_count: 0,
            links: Vec::new(),
            dropped_links_count: 0,
            status: None,
        }
    }

    #[test]
    fn ids_are_hex_timestamps_are_strings_kind_is_int() {
        let j = span_json(base_span());
        assert_eq!(j["traceId"], "5b8efff798038103d269b633813fc60c");
        assert_eq!(j["spanId"], "eee19b7ec3c1b174");
        assert_eq!(j["startTimeUnixNano"], "1544712660000000000");
        assert_eq!(j["endTimeUnixNano"], "1544712661000000000");
        assert_eq!(j["kind"], 2);
        // proto3 defaults omitted
        assert!(j.get("parentSpanId").is_none());
        assert!(j.get("traceState").is_none());
        assert!(j.get("flags").is_none());
        assert!(j.get("attributes").is_none());
        assert!(j.get("status").is_none());
    }

    #[test]
    fn int_value_is_string_bytes_value_is_base64() {
        let mut s = base_span();
        s.attributes = vec![
            KeyValue {
                key: "count".into(),
                value: Some(AnyValue {
                    value: Some(PV::IntValue(42)),
                }),
                key_ref: 0,
            },
            KeyValue {
                key: "blob".into(),
                value: Some(AnyValue {
                    value: Some(PV::BytesValue(vec![1, 2, 3])),
                }),
                key_ref: 0,
            },
            KeyValue {
                key: "name".into(),
                value: Some(AnyValue {
                    value: Some(PV::StringValue("v".into())),
                }),
                key_ref: 0,
            },
        ];
        let j = span_json(s);
        let attrs = j["attributes"].as_array().unwrap();
        let by = |k: &str| attrs.iter().find(|a| a["key"] == k).unwrap()["value"].clone();
        assert_eq!(by("count")["intValue"], "42"); // int64 as STRING
        assert_eq!(by("blob")["bytesValue"], "AQID"); // base64
        assert_eq!(by("name")["stringValue"], "v");
    }

    #[test]
    fn status_and_parent_and_link_emitted() {
        let mut s = base_span();
        s.parent_span_id = 0xEEE19B7EC3C1B173u64.to_be_bytes().to_vec();
        s.status = Some(Status {
            message: "boom".into(),
            code: 2,
        });
        s.links = vec![Link {
            trace_id: 1u128.to_be_bytes().to_vec(),
            span_id: 2u64.to_be_bytes().to_vec(),
            trace_state: String::new(),
            attributes: Vec::new(),
            dropped_attributes_count: 0,
            flags: 1,
        }];
        let j = span_json(s);
        assert_eq!(j["parentSpanId"], "eee19b7ec3c1b173");
        assert_eq!(j["status"]["code"], 2);
        assert_eq!(j["status"]["message"], "boom");
        assert_eq!(j["links"][0]["traceId"], "00000000000000000000000000000001");
        assert_eq!(j["links"][0]["spanId"], "0000000000000002");
        assert_eq!(j["links"][0]["flags"], 1); // link flags must be preserved
    }
}
