// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Maps Datadog trace/spans directly to the generated prost OTLP types for HTTP/protobuf export,
//! sharing all semantic decisions with the JSON mapper via the neutral helpers in `mapper`.

use super::mapper::{
    chunk_trace_id_high, collect_event_attributes, collect_span_attributes, span_kind, span_status,
    AttrValue,
};
use super::OtlpResourceInfo;
use crate::span::v04::{Span, SpanEvent, SpanLink};
use crate::span::TraceData;
use std::borrow::Borrow;

use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value::Value as ProtoValue, AnyValue as ProtoAnyValue, ArrayValue as ProtoArrayValue,
    InstrumentationScope as ProtoScope, KeyValue as ProtoKeyValue,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource as ProtoResource;
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
    span::{Event as ProtoEvent, Link as ProtoLink},
    ResourceSpans as ProtoResourceSpans, ScopeSpans as ProtoScopeSpans, Span as ProtoSpan,
    Status as ProtoStatus,
};

fn proto_value(v: AttrValue) -> ProtoAnyValue {
    let value = match v {
        AttrValue::Str(s) => ProtoValue::StringValue(s),
        AttrValue::Bool(b) => ProtoValue::BoolValue(b),
        AttrValue::Int(i) => ProtoValue::IntValue(i),
        AttrValue::Double(d) => ProtoValue::DoubleValue(d),
        AttrValue::Bytes(b) => ProtoValue::BytesValue(b),
        AttrValue::Array(items) => ProtoValue::ArrayValue(ProtoArrayValue {
            values: items.into_iter().map(proto_value).collect(),
        }),
    };
    ProtoAnyValue { value: Some(value) }
}

fn proto_kv((key, value): (String, AttrValue)) -> ProtoKeyValue {
    // `key_ref` is a profiling-signal-only field; explicit zero (no `..Default::default()`).
    ProtoKeyValue {
        key,
        value: Some(proto_value(value)),
        key_ref: 0,
    }
}

/// Maps Datadog trace chunks to a prost `ExportTraceServiceRequest`, built directly from the
/// native span fields (no `json_types` intermediate, no hex/decimal round trip).
pub fn map_traces_to_otlp_proto<T: TraceData>(
    trace_chunks: Vec<Vec<Span<T>>>,
    resource_info: &OtlpResourceInfo,
) -> ProtoReq {
    let resource = build_proto_resource(resource_info);
    let mut all_spans: Vec<ProtoSpan> = Vec::new();
    for chunk in &trace_chunks {
        let high = chunk_trace_id_high(chunk);
        for span in chunk {
            all_spans.push(map_span_proto(span, &resource_info.service, high));
        }
    }
    ProtoReq {
        resource_spans: vec![ProtoResourceSpans {
            resource: Some(resource),
            scope_spans: vec![ProtoScopeSpans {
                scope: Some(ProtoScope::default()),
                spans: all_spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

fn push_str_attr(attrs: &mut Vec<ProtoKeyValue>, k: &str, v: &str) {
    if !v.is_empty() {
        attrs.push(proto_kv((k.to_string(), AttrValue::Str(v.to_string()))));
    }
}

fn build_proto_resource(resource_info: &OtlpResourceInfo) -> ProtoResource {
    let mut attributes: Vec<ProtoKeyValue> = Vec::new();
    push_str_attr(&mut attributes, "service.name", &resource_info.service);
    push_str_attr(
        &mut attributes,
        "deployment.environment.name",
        &resource_info.env,
    );
    push_str_attr(
        &mut attributes,
        "service.version",
        &resource_info.app_version,
    );
    attributes.push(proto_kv((
        "telemetry.sdk.name".to_string(),
        AttrValue::Str("datadog".to_string()),
    )));
    push_str_attr(
        &mut attributes,
        "telemetry.sdk.language",
        &resource_info.language,
    );
    push_str_attr(
        &mut attributes,
        "telemetry.sdk.version",
        &resource_info.tracer_version,
    );
    push_str_attr(&mut attributes, "runtime-id", &resource_info.runtime_id);
    // `entity_refs` is a profiling-signal-only field; explicit default.
    ProtoResource {
        attributes,
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
    }
}

fn map_span_proto<T: TraceData>(
    span: &Span<T>,
    resource_service: &str,
    chunk_trace_id_high: u64,
) -> ProtoSpan {
    let trace_id_128 = ((chunk_trace_id_high as u128) << 64) | (span.trace_id as u64 as u128);
    let parent_span_id = if span.parent_id != 0 {
        span.parent_id.to_be_bytes().to_vec()
    } else {
        Vec::new()
    };
    let (attrs, dropped_attributes_count) = collect_span_attributes(span, resource_service);
    let attributes = attrs.into_iter().map(proto_kv).collect();
    let (code, message) = span_status(span);
    let flags = span
        .metrics
        .get("_sampling_priority_v1")
        .map(|p| if *p >= 1.0 { 1u32 } else { 0u32 })
        .unwrap_or(0);
    let trace_state = span
        .meta
        .get("tracestate")
        .map(|v| v.borrow().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let links = span.span_links.iter().map(map_span_link_proto).collect();
    let (events, dropped_events_count) = map_span_events_proto(&span.span_events);
    ProtoSpan {
        trace_id: trace_id_128.to_be_bytes().to_vec(),
        span_id: span.span_id.to_be_bytes().to_vec(),
        trace_state,
        parent_span_id,
        flags,
        name: span.resource.borrow().to_string(),
        kind: span_kind(span),
        start_time_unix_nano: span.start as u64,
        end_time_unix_nano: (span.start + span.duration) as u64,
        attributes,
        dropped_attributes_count: dropped_attributes_count as u32,
        events,
        dropped_events_count: dropped_events_count as u32,
        links,
        // The mapper enforces no link cap, so dropped links is always 0.
        dropped_links_count: 0,
        status: Some(ProtoStatus {
            message: message.unwrap_or_default(),
            code,
        }),
    }
}

fn map_span_link_proto<T: TraceData>(link: &SpanLink<T>) -> ProtoLink {
    let trace_id_128 = ((link.trace_id_high as u128) << 64) | (link.trace_id as u128);
    let attributes = link
        .attributes
        .iter()
        .map(|(k, v)| {
            proto_kv((
                k.borrow().to_string(),
                AttrValue::Str(v.borrow().to_string()),
            ))
        })
        .collect();
    ProtoLink {
        trace_id: trace_id_128.to_be_bytes().to_vec(),
        span_id: link.span_id.to_be_bytes().to_vec(),
        trace_state: {
            let ts = link.tracestate.borrow();
            if ts.is_empty() {
                String::new()
            } else {
                ts.to_string()
            }
        },
        attributes,
        dropped_attributes_count: 0,
        // `SpanLink` has no flags field; faithful value is 0.
        flags: 0,
    }
}

fn map_span_events_proto<T: TraceData>(events: &[SpanEvent<T>]) -> (Vec<ProtoEvent>, usize) {
    const MAX_EVENTS_PER_SPAN: usize = 128;
    let mut out = Vec::with_capacity(events.len().min(MAX_EVENTS_PER_SPAN));
    for ev in events.iter().take(MAX_EVENTS_PER_SPAN) {
        out.push(ProtoEvent {
            time_unix_nano: ev.time_unix_nano,
            name: ev.name.borrow().to_string(),
            attributes: collect_event_attributes(ev)
                .into_iter()
                .map(proto_kv)
                .collect(),
            dropped_attributes_count: 0,
        });
    }
    let dropped = events.len().saturating_sub(out.len());
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::BytesData;

    #[test]
    fn proto_span_uses_raw_id_bytes_and_native_timestamps() {
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 0x5b8efff798038103_d269b633813fc60c_u128,
            span_id: 0xEEE19B7EC3C1B174,
            parent_id: 0xEEE19B7EC3C1B173,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 1544712660000000000,
            duration: 1000000000,
            ..Default::default()
        };
        let req = map_traces_to_otlp_proto(vec![vec![span]], &resource_info);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(
            s.trace_id,
            0x5b8efff798038103_d269b633813fc60c_u128
                .to_be_bytes()
                .to_vec()
        );
        assert_eq!(s.span_id, 0xEEE19B7EC3C1B174u64.to_be_bytes().to_vec());
        assert_eq!(
            s.parent_span_id,
            0xEEE19B7EC3C1B173u64.to_be_bytes().to_vec()
        );
        assert_eq!(s.start_time_unix_nano, 1544712660000000000);
        assert_eq!(s.end_time_unix_nano, 1544712661000000000);
        assert_eq!(s.name, "res");
    }
}
