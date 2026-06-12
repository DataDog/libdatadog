// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Converts the hand-rolled serde OTLP request (the JSON wire model) into the generated
//! prost types for binary (HTTP/protobuf) export. The semantic DD-span -> OTLP mapping already
//! happened in `mapper.rs`; this is a purely structural translation.

use crate::otlp_encoder::json_types as j;
use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value::Value as ProtoValue, AnyValue as ProtoAnyValue, ArrayValue as ProtoArrayValue,
    InstrumentationScope as ProtoScope, KeyValue as ProtoKeyValue,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource as ProtoResource;
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
    span::{Event as ProtoEvent, Link as ProtoLink},
    status::StatusCode as ProtoStatusCode,
    ResourceSpans as ProtoResourceSpans, ScopeSpans as ProtoScopeSpans, Span as ProtoSpan,
    Status as ProtoStatus,
};

/// Decode a fixed-width lowercase hex string into a byte vector. The mapper always produces
/// well-formed hex of the expected width; on a malformed value we fall back to an all-zero
/// buffer of `len` bytes rather than panicking (FFI reliability).
fn hex_to_bytes(s: &str, len: usize) -> Vec<u8> {
    let bytes = s.as_bytes();
    if bytes.len() != len * 2 {
        return vec![0u8; len];
    }
    let mut out = Vec::with_capacity(len);
    let mut i = 0;
    while i < bytes.len() {
        match (hex_nibble(bytes[i]), hex_nibble(bytes[i + 1])) {
            (Some(hi), Some(lo)) => out.push((hi << 4) | lo),
            _ => return vec![0u8; len],
        }
        i += 2;
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_u64(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

impl From<&j::AnyValue> for ProtoAnyValue {
    fn from(v: &j::AnyValue) -> Self {
        let value = match v {
            j::AnyValue::StringValue(s) => ProtoValue::StringValue(s.clone()),
            j::AnyValue::BoolValue(b) => ProtoValue::BoolValue(*b),
            j::AnyValue::IntValue(i) => ProtoValue::IntValue(*i),
            j::AnyValue::DoubleValue(d) => ProtoValue::DoubleValue(*d),
            j::AnyValue::BytesValue(b) => ProtoValue::BytesValue(b.clone()),
            j::AnyValue::ArrayValue(a) => ProtoValue::ArrayValue(ProtoArrayValue {
                values: a.values.iter().map(ProtoAnyValue::from).collect(),
            }),
        };
        ProtoAnyValue { value: Some(value) }
    }
}

fn kv(k: &j::KeyValue) -> ProtoKeyValue {
    ProtoKeyValue {
        key: k.key.clone(),
        value: Some(ProtoAnyValue::from(&k.value)),
        key_ref: 0,
    }
}

impl From<&j::ExportTraceServiceRequest> for ProtoReq {
    fn from(req: &j::ExportTraceServiceRequest) -> Self {
        ProtoReq {
            resource_spans: req.resource_spans.iter().map(resource_spans).collect(),
        }
    }
}

fn resource_spans(rs: &j::ResourceSpans) -> ProtoResourceSpans {
    ProtoResourceSpans {
        resource: rs.resource.as_ref().map(|r| ProtoResource {
            attributes: r.attributes.iter().map(kv).collect(),
            dropped_attributes_count: 0,
            entity_refs: Vec::new(),
        }),
        scope_spans: rs.scope_spans.iter().map(scope_spans).collect(),
        schema_url: String::new(),
    }
}

fn scope_spans(ss: &j::ScopeSpans) -> ProtoScopeSpans {
    ProtoScopeSpans {
        scope: ss.scope.as_ref().map(|s| ProtoScope {
            name: s.name.clone().unwrap_or_default(),
            version: s.version.clone().unwrap_or_default(),
            attributes: Vec::new(),
            dropped_attributes_count: 0,
        }),
        spans: ss.spans.iter().map(span).collect(),
        schema_url: ss.schema_url.clone().unwrap_or_default(),
    }
}

fn span(s: &j::OtlpSpan) -> ProtoSpan {
    ProtoSpan {
        trace_id: hex_to_bytes(&s.trace_id, 16),
        span_id: hex_to_bytes(&s.span_id, 8),
        trace_state: s.trace_state.clone().unwrap_or_default(),
        parent_span_id: s
            .parent_span_id
            .as_ref()
            .map(|p| hex_to_bytes(p, 8))
            .unwrap_or_default(),
        flags: s.flags.unwrap_or(0),
        name: s.name.clone(),
        kind: s.kind,
        start_time_unix_nano: parse_u64(&s.start_time_unix_nano),
        end_time_unix_nano: parse_u64(&s.end_time_unix_nano),
        attributes: s.attributes.iter().map(kv).collect(),
        dropped_attributes_count: s.dropped_attributes_count.unwrap_or(0),
        events: s.events.iter().map(event).collect(),
        dropped_events_count: s.dropped_events_count.unwrap_or(0),
        links: s.links.iter().map(link).collect(),
        dropped_links_count: 0,
        status: Some(ProtoStatus {
            message: s.status.message.clone().unwrap_or_default(),
            code: status_code(s.status.code),
        }),
    }
}

fn status_code(code: i32) -> i32 {
    match code {
        c if c == j::status_code::OK => ProtoStatusCode::Ok as i32,
        c if c == j::status_code::ERROR => ProtoStatusCode::Error as i32,
        _ => ProtoStatusCode::Unset as i32,
    }
}

fn link(l: &j::OtlpSpanLink) -> ProtoLink {
    ProtoLink {
        trace_id: hex_to_bytes(&l.trace_id, 16),
        span_id: hex_to_bytes(&l.span_id, 8),
        trace_state: l.trace_state.clone().unwrap_or_default(),
        attributes: l.attributes.iter().map(kv).collect(),
        dropped_attributes_count: l.dropped_attributes_count.unwrap_or(0),
        flags: 0,
    }
}

fn event(e: &j::OtlpSpanEvent) -> ProtoEvent {
    ProtoEvent {
        time_unix_nano: parse_u64(&e.time_unix_nano),
        name: e.name.clone(),
        attributes: e.attributes.iter().map(kv).collect(),
        dropped_attributes_count: e.dropped_attributes_count.unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use crate::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
    use crate::span::BytesData;
    use crate::span::v04::Span;
    use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;

    #[test]
    fn converts_ids_and_attributes_to_proto() {
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let mut span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 0xEEE19B7EC3C1B174,
            parent_id: 0xEEE19B7EC3C1B173,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 1544712660000000000,
            duration: 1000000000,
            error: 0,
            ..Default::default()
        };
        span.metrics
            .insert(libdd_tinybytes::BytesString::from_static("count"), 42.0);

        let serde_req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let proto: ProtoReq = (&serde_req).into();

        let rs = &proto.resource_spans[0];
        let sp = &rs.scope_spans[0].spans[0];
        assert_eq!(
            sp.trace_id,
            vec![0, 0, 0, 0, 0, 0, 0, 0, 0xD2, 0x69, 0xB6, 0x33, 0x81, 0x3F, 0xC6, 0x0C]
        );
        assert_eq!(sp.span_id, vec![0xEE, 0xE1, 0x9B, 0x7E, 0xC3, 0xC1, 0xB1, 0x74]);
        assert_eq!(
            sp.parent_span_id,
            vec![0xEE, 0xE1, 0x9B, 0x7E, 0xC3, 0xC1, 0xB1, 0x73]
        );
        assert_eq!(sp.name, "res");
        assert_eq!(sp.start_time_unix_nano, 1544712660000000000);
        assert_eq!(sp.end_time_unix_nano, 1544712661000000000);
        let count = sp
            .attributes
            .iter()
            .find(|kv| kv.key == "count")
            .expect("count attr");
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value;
        assert!(matches!(
            count.value.as_ref().unwrap().value,
            Some(Value::IntValue(42))
        ));
    }
}
