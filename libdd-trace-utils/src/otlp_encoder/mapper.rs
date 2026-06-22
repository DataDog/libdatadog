// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Maps Datadog trace/spans to OTLP ExportTraceServiceRequest.

use super::json_types::{
    self, AnyValue, ExportTraceServiceRequest, InstrumentationScope, KeyValue, OtlpSpan,
    OtlpSpanEvent, OtlpSpanLink, Resource, ResourceSpans, ScopeSpans, Status,
};
use super::OtlpResourceInfo;
use crate::span::v04::{Span, SpanEvent, SpanLink};
use crate::span::TraceData;
use std::borrow::Borrow;

/// Maximum number of attributes per span; excess are dropped and counted.
const MAX_ATTRIBUTES_PER_SPAN: usize = 128;

/// Maps Datadog trace chunks and resource info to an OTLP ExportTraceServiceRequest.
///
/// Resource: SDK-level attributes (service.name, deployment.environment.name, telemetry.sdk.*,
/// runtime-id). InstrumentationScope: present but empty (DD SDKs don't have a scope concept).
/// All analogous DD span fields are mapped; meta→attributes (string), metrics→attributes
/// (int/double), links and events mapped to OTLP links and events. Status from span.error and
/// meta["error.msg"].
///
/// The high 64 bits of a 128-bit trace ID are carried in the trace_id field itself or (if not
/// present) as the `_dd.p.tid` meta tag, which per RFC #85 is set on the chunk root only.
/// We resolve it once per chunk and apply it to every span so OTLP receivers see the full 128-bit
/// trace_id on every span in the trace.
pub fn map_traces_to_otlp<T: TraceData>(
    trace_chunks: Vec<Vec<Span<T>>>,
    resource_info: &OtlpResourceInfo,
) -> ExportTraceServiceRequest {
    let resource = build_resource(resource_info);
    let mut all_spans: Vec<OtlpSpan> = Vec::new();
    for chunk in &trace_chunks {
        // Resolve the high 64 bits of the 128-bit trace ID once per chunk. For each span,
        // prefer the native u128 `trace_id` field (e.g. Python's native spans hold the full
        // 128-bit ID there) and fall back to its RFC #85 `_dd.p.tid` meta tag.
        let chunk_trace_id_high: u64 = chunk
            .iter()
            .find_map(|s| {
                let high = (s.trace_id >> 64) as u64;
                if high != 0 {
                    return Some(high);
                }
                s.meta
                    .get("_dd.p.tid")
                    .and_then(|v| u64::from_str_radix(v.borrow(), 16).ok())
            })
            .unwrap_or(0);
        for span in chunk {
            all_spans.push(map_span(span, &resource_info.service, chunk_trace_id_high));
        }
    }
    let scope_spans = ScopeSpans {
        scope: Some(InstrumentationScope::default()),
        spans: all_spans,
        schema_url: None,
    };
    let resource_spans = ResourceSpans {
        resource: Some(resource),
        scope_spans: vec![scope_spans],
    };
    ExportTraceServiceRequest {
        resource_spans: vec![resource_spans],
    }
}

fn build_resource(resource_info: &OtlpResourceInfo) -> Resource {
    let mut attributes: Vec<KeyValue> = Vec::new();
    if !resource_info.service.is_empty() {
        attributes.push(KeyValue {
            key: "service.name".to_string(),
            value: AnyValue::StringValue(resource_info.service.clone()),
        });
    }
    if !resource_info.env.is_empty() {
        attributes.push(KeyValue {
            key: "deployment.environment.name".to_string(),
            value: AnyValue::StringValue(resource_info.env.clone()),
        });
    }
    if !resource_info.app_version.is_empty() {
        attributes.push(KeyValue {
            key: "service.version".to_string(),
            value: AnyValue::StringValue(resource_info.app_version.clone()),
        });
    }
    attributes.push(KeyValue {
        key: "telemetry.sdk.name".to_string(),
        value: AnyValue::StringValue("datadog".to_string()),
    });
    if !resource_info.language.is_empty() {
        attributes.push(KeyValue {
            key: "telemetry.sdk.language".to_string(),
            value: AnyValue::StringValue(resource_info.language.clone()),
        });
    }
    if !resource_info.tracer_version.is_empty() {
        attributes.push(KeyValue {
            key: "telemetry.sdk.version".to_string(),
            value: AnyValue::StringValue(resource_info.tracer_version.clone()),
        });
    }
    if !resource_info.runtime_id.is_empty() {
        attributes.push(KeyValue {
            key: "runtime-id".to_string(),
            value: AnyValue::StringValue(resource_info.runtime_id.clone()),
        });
    }
    // Tells Datadog Agent OTLP receivers to skip their concentrator; prevents double-counted APM
    // metrics.
    if resource_info.client_computed_stats {
        attributes.push(KeyValue {
            key: "_dd.stats_computed".to_string(),
            value: AnyValue::StringValue("true".to_string()),
        });
    }
    Resource { attributes }
}

fn map_span<T: TraceData>(
    span: &Span<T>,
    resource_service: &str,
    chunk_trace_id_high: u64,
) -> OtlpSpan {
    // Reconstruct the full 128-bit trace ID. The caller resolves the high 64 bits once per
    // chunk (from either the native u128 `trace_id` field or the "_dd.p.tid" meta tag).
    // All spans in a chunk share the same trace ID.
    let trace_id_128 = ((chunk_trace_id_high as u128) << 64) | (span.trace_id as u64 as u128);
    let trace_id_hex = format!("{:032x}", trace_id_128);
    let span_id_hex = format!("{:016x}", span.span_id);
    let parent_span_id = if span.parent_id != 0 {
        Some(format!("{:016x}", span.parent_id))
    } else {
        None
    };
    let start_nano = span.start;
    let end_nano = span.start + span.duration;
    let start_time_unix_nano = start_nano.to_string();
    let end_time_unix_nano = end_nano.to_string();
    // Prefer explicit "span.kind" tag (set by OTEL-instrumented tracers); fall back to
    // the Datadog span type field for DD-instrumented spans.
    let kind = span
        .meta
        .get("span.kind")
        .map(|v| tag_to_otlp_kind(v.borrow()))
        .unwrap_or_else(|| dd_type_to_otlp_kind(span.r#type.borrow()));
    let (attributes, dropped_attributes_count) = map_attributes(span, resource_service);
    let error_msg = span.meta.get("error.msg").map(|v| v.borrow().to_string());
    let status = if span.error != 0 {
        Status {
            message: error_msg,
            code: json_types::status_code::ERROR,
        }
    } else {
        Status {
            message: None,
            code: json_types::status_code::UNSET,
        }
    };
    // Set flags from sampling priority: 1 = sampled/keep, 0 = dropped.
    let flags = span
        .metrics
        .get("_sampling_priority_v1")
        .map(|p| if *p >= 1.0 { 1u32 } else { 0u32 });
    let trace_state = span
        .meta
        .get("tracestate")
        .map(|v| v.borrow().to_string())
        .filter(|s| !s.is_empty());
    let links = span.span_links.iter().map(map_span_link).collect();
    let (events, dropped_events_count) = map_span_events(&span.span_events);
    OtlpSpan {
        trace_id: trace_id_hex,
        span_id: span_id_hex,
        parent_span_id,
        trace_state,
        name: span.resource.borrow().to_string(),
        kind,
        start_time_unix_nano,
        end_time_unix_nano,
        attributes,
        status,
        links,
        events,
        dropped_attributes_count: if dropped_attributes_count > 0 {
            Some(dropped_attributes_count as u32)
        } else {
            None
        },
        dropped_events_count: if dropped_events_count > 0 {
            Some(dropped_events_count as u32)
        } else {
            None
        },
        flags,
    }
}

fn map_span_link<T: TraceData>(link: &SpanLink<T>) -> OtlpSpanLink {
    let trace_id_128 = ((link.trace_id_high as u128) << 64) | (link.trace_id as u128);
    let trace_id_hex = format!("{:032x}", trace_id_128);
    let span_id_hex = format!("{:016x}", link.span_id);
    let trace_state = if link.tracestate.borrow().is_empty() {
        None
    } else {
        Some(link.tracestate.borrow().to_string())
    };
    let attributes: Vec<KeyValue> = link
        .attributes
        .iter()
        .map(|(k, v)| KeyValue {
            key: k.borrow().to_string(),
            value: AnyValue::StringValue(v.borrow().to_string()),
        })
        .collect();
    OtlpSpanLink {
        trace_id: trace_id_hex,
        span_id: span_id_hex,
        trace_state,
        attributes,
        dropped_attributes_count: None,
    }
}

fn map_span_events<T: TraceData>(events: &[SpanEvent<T>]) -> (Vec<OtlpSpanEvent>, usize) {
    const MAX_EVENTS_PER_SPAN: usize = 128;
    let mut otlp_events = Vec::with_capacity(events.len().min(MAX_EVENTS_PER_SPAN));
    for ev in events.iter().take(MAX_EVENTS_PER_SPAN) {
        let attributes: Vec<KeyValue> = ev
            .attributes
            .iter()
            .map(|(k, v)| event_attr_to_key_value(k, v))
            .collect();
        otlp_events.push(OtlpSpanEvent {
            time_unix_nano: ev.time_unix_nano.to_string(),
            name: ev.name.borrow().to_string(),
            attributes,
            dropped_attributes_count: None,
        });
    }
    let dropped = events.len().saturating_sub(otlp_events.len());
    (otlp_events, dropped)
}

fn event_attr_to_key_value<T: TraceData>(
    k: &T::Text,
    v: &crate::span::v04::AttributeAnyValue<T>,
) -> KeyValue {
    use crate::span::v04::AttributeArrayValue;
    let value = match v {
        crate::span::v04::AttributeAnyValue::SingleValue(av) => match av {
            AttributeArrayValue::String(s) => AnyValue::StringValue(s.borrow().to_string()),
            AttributeArrayValue::Boolean(b) => AnyValue::BoolValue(*b),
            AttributeArrayValue::Integer(i) => AnyValue::IntValue(*i),
            AttributeArrayValue::Double(d) => AnyValue::DoubleValue(*d),
        },
        crate::span::v04::AttributeAnyValue::Array(items) => {
            let values = items
                .iter()
                .map(|item| match item {
                    AttributeArrayValue::String(s) => AnyValue::StringValue(s.borrow().to_string()),
                    AttributeArrayValue::Boolean(b) => AnyValue::BoolValue(*b),
                    AttributeArrayValue::Integer(i) => AnyValue::IntValue(*i),
                    AttributeArrayValue::Double(d) => AnyValue::DoubleValue(*d),
                })
                .collect();
            AnyValue::ArrayValue(crate::otlp_encoder::json_types::ArrayValue { values })
        }
    };
    KeyValue {
        key: k.borrow().to_string(),
        value,
    }
}

/// Maps the explicit "span.kind" meta tag (set by OTEL-instrumented tracers) to an OTLP SpanKind.
fn tag_to_otlp_kind(t: &str) -> i32 {
    match t.to_lowercase().as_str() {
        "server" => json_types::span_kind::SERVER,
        "client" => json_types::span_kind::CLIENT,
        "producer" => json_types::span_kind::PRODUCER,
        "consumer" => json_types::span_kind::CONSUMER,
        "internal" => json_types::span_kind::INTERNAL,
        _ => json_types::span_kind::UNSPECIFIED,
    }
}

/// Maps the Datadog span type field (set by DD-instrumented tracers) to an OTLP SpanKind.
fn dd_type_to_otlp_kind(t: &str) -> i32 {
    match t.to_lowercase().as_str() {
        "server" | "web" | "http" => json_types::span_kind::SERVER,
        "client" => json_types::span_kind::CLIENT,
        "producer" => json_types::span_kind::PRODUCER,
        "consumer" => json_types::span_kind::CONSUMER,
        _ => json_types::span_kind::INTERNAL,
    }
}

fn map_attributes<T: TraceData>(span: &Span<T>, resource_service: &str) -> (Vec<KeyValue>, usize) {
    let mut attrs: Vec<KeyValue> = Vec::new();
    // Add service.name when the span's service differs from the resource-level service.
    let span_service = span.service.borrow();
    let has_per_span_service = !span_service.is_empty() && span_service != resource_service;
    if has_per_span_service {
        attrs.push(KeyValue {
            key: "service.name".to_string(),
            value: AnyValue::StringValue(span_service.to_string()),
        });
    }
    let operation_name = span.name.borrow();
    let has_operation_name = !operation_name.is_empty();
    if has_operation_name {
        attrs.push(KeyValue {
            key: "operation.name".to_string(),
            value: AnyValue::StringValue(operation_name.to_string()),
        });
    }
    let span_type = span.r#type.borrow();
    let has_span_type = !span_type.is_empty();
    if has_span_type {
        attrs.push(KeyValue {
            key: "span.type".to_string(),
            value: AnyValue::StringValue(span_type.to_string()),
        });
    }
    let resource_name = span.resource.borrow();
    let has_resource_name = !resource_name.is_empty();
    if has_resource_name {
        attrs.push(KeyValue {
            key: "resource.name".to_string(),
            value: AnyValue::StringValue(resource_name.to_string()),
        });
    }
    for (k, v) in span.meta.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        attrs.push(KeyValue {
            key: k.borrow().to_string(),
            value: AnyValue::StringValue(v.borrow().to_string()),
        });
    }
    for (k, v) in span.metrics.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        let value = if v.fract() == 0.0 && (*v >= i64::MIN as f64 && *v <= i64::MAX as f64) {
            AnyValue::IntValue(*v as i64)
        } else {
            AnyValue::DoubleValue(*v)
        };
        attrs.push(KeyValue {
            key: k.borrow().to_string(),
            value,
        });
    }
    for (k, v) in span.meta_struct.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        attrs.push(KeyValue {
            key: k.borrow().to_string(),
            value: AnyValue::BytesValue(v.borrow().to_vec()),
        });
    }
    let total = (if has_per_span_service { 1 } else { 0 })
        + (if has_operation_name { 1 } else { 0 })
        + (if has_span_type { 1 } else { 0 })
        + (if has_resource_name { 1 } else { 0 })
        + span.meta.len()
        + span.metrics.len()
        + span.meta_struct.len();
    let dropped = total.saturating_sub(attrs.len());
    (attrs, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otlp_encoder::OtlpResourceInfo;
    use crate::span::BytesData;

    #[test]
    fn test_trace_id_span_id_format() {
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128, // low 64 bits only (v04 wire format)
            span_id: 0xEEE19B7EC3C1B174,
            parent_id: 0xEEE19B7EC3C1B173,
            name: libdd_tinybytes::BytesString::from_static("test"),
            service: libdd_tinybytes::BytesString::from_static("svc"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 1544712660000000000,
            duration: 1000000000,
            error: 0,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let rs = &req.resource_spans[0];
        let otlp_span = &rs.scope_spans[0].spans[0];
        assert_eq!(otlp_span.trace_id, "0000000000000000d269b633813fc60c");
        assert_eq!(otlp_span.span_id, "eee19b7ec3c1b174");
        assert_eq!(
            otlp_span.parent_span_id.as_deref(),
            Some("eee19b7ec3c1b173")
        );
        assert_eq!(otlp_span.kind, json_types::span_kind::SERVER);
        assert_eq!(otlp_span.start_time_unix_nano, "1544712660000000000");
        assert_eq!(otlp_span.end_time_unix_nano, "1544712661000000000");
        assert_eq!(rs.scope_spans[0].scope.as_ref().unwrap().name, None);
    }

    #[test]
    fn test_status_error_message_from_meta() {
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("err_span"),
            start: 0,
            duration: 1,
            error: 1,
            ..Default::default()
        };
        span.meta.insert(
            libdd_tinybytes::BytesString::from_static("error.msg"),
            libdd_tinybytes::BytesString::from_static("something broke"),
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        let status = &otlp_span.status;
        assert_eq!(status.code, json_types::status_code::ERROR);
        assert_eq!(status.message.as_deref(), Some("something broke"));
    }

    #[test]
    fn test_metrics_as_int_or_double() {
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("m"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.metrics
            .insert(libdd_tinybytes::BytesString::from_static("count"), 42.0);
        span.metrics.insert(
            libdd_tinybytes::BytesString::from_static("rate"),
            std::f64::consts::PI,
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        let count_kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "count")
            .unwrap();
        assert_eq!(count_kv["value"]["intValue"], "42");
        let rate_kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "rate")
            .unwrap();
        let rate = rate_kv["value"]["doubleValue"].as_f64().unwrap();
        assert!((rate - std::f64::consts::PI).abs() < 1e-9);
    }

    #[test]
    fn test_128bit_trace_id_from_dd_p_tid() {
        // When "_dd.p.tid" is present it supplies the high 64 bits of the trace ID.
        // Low 64 bits come from span.trace_id; the two are concatenated to form a 128-bit hex ID.
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128, // low 64 bits
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("5b8efff798038103"),
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(otlp_span.trace_id, "5b8efff798038103d269b633813fc60c");
    }

    #[test]
    fn test_128bit_trace_id_from_native_span_field() {
        // When the span's u128 `trace_id` field already carries the full 128-bit ID (e.g.
        // tracers with native spans like Python), the chunk-root meta lookup is skipped and
        // the field's high 64 bits are propagated to every span in the chunk.
        let resource_info = OtlpResourceInfo::default();
        let full: u128 = 0x5b8efff798038103_d269b633813fc60c_u128;
        let root: Span<BytesData> = Span {
            trace_id: full,
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("root"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        // Child carries only the low 64 bits; it should still inherit the chunk's high bits.
        let child: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 2,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![root, child]], &resource_info);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        let expected = "5b8efff798038103d269b633813fc60c";
        assert_eq!(spans[0].trace_id, expected);
        assert_eq!(spans[1].trace_id, expected);
    }

    #[test]
    fn test_128bit_trace_id_without_dd_p_tid() {
        // When the entire chunk has no "_dd.p.tid" the high 64 bits default to zero
        // (legacy 64-bit-only trace IDs).
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(otlp_span.trace_id, "0000000000000000d269b633813fc60c");
    }

    #[test]
    fn test_128bit_trace_id_propagated_to_chunk_children() {
        // Per RFC #85 dd-trace tracers set "_dd.p.tid" only on the chunk root.
        // The OTLP mapper must apply that high-bits value to every span in the chunk
        // so receivers see the full 128-bit trace_id on every span.
        let resource_info = OtlpResourceInfo::default();
        let low: u128 = 0xD269B633813FC60C_u128;
        let mut root: Span<BytesData> = Span {
            trace_id: low,
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("root"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        root.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("5b8efff798038103"),
        );
        let child_a: Span<BytesData> = Span {
            trace_id: low,
            span_id: 2,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child_a"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let child_b: Span<BytesData> = Span {
            trace_id: low,
            span_id: 3,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child_b"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![root, child_a, child_b]], &resource_info);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 3);
        let expected = "5b8efff798038103d269b633813fc60c";
        for s in spans {
            assert_eq!(s.trace_id, expected, "span {} mismatched", s.span_id);
        }
    }

    #[test]
    fn test_128bit_trace_id_isolation_across_chunks() {
        // The chunk-level high bits must not leak across chunks. Each chunk's spans
        // get only their own chunk root's "_dd.p.tid".
        let resource_info = OtlpResourceInfo::default();
        let low_a: u128 = 0x1111111111111111_u128;
        let low_b: u128 = 0x2222222222222222_u128;
        let mut root_a: Span<BytesData> = Span {
            trace_id: low_a,
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("root_a"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        root_a.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("aaaaaaaaaaaaaaaa"),
        );
        let child_a: Span<BytesData> = Span {
            trace_id: low_a,
            span_id: 2,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child_a"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let mut root_b: Span<BytesData> = Span {
            trace_id: low_b,
            span_id: 3,
            name: libdd_tinybytes::BytesString::from_static("root_b"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        root_b.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("bbbbbbbbbbbbbbbb"),
        );
        let child_b: Span<BytesData> = Span {
            trace_id: low_b,
            span_id: 4,
            parent_id: 3,
            name: libdd_tinybytes::BytesString::from_static("child_b"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(
            vec![vec![root_a, child_a], vec![root_b, child_b]],
            &resource_info,
        );
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 4);
        // Spans 1, 2 belong to chunk A; spans 3, 4 to chunk B.
        let expect_a = "aaaaaaaaaaaaaaaa1111111111111111";
        let expect_b = "bbbbbbbbbbbbbbbb2222222222222222";
        assert_eq!(spans[0].trace_id, expect_a);
        assert_eq!(spans[1].trace_id, expect_a);
        assert_eq!(spans[2].trace_id, expect_b);
        assert_eq!(spans[3].trace_id, expect_b);
    }

    #[test]
    fn test_chunk_with_malformed_dd_p_tid_on_root_falls_back() {
        // If the chunk root's "_dd.p.tid" fails to parse, the scan continues looking for
        // any other parseable value in the chunk before giving up. This keeps a malformed
        // tag on one span from poisoning the rest of the trace.
        let resource_info = OtlpResourceInfo::default();
        let low: u128 = 0xD269B633813FC60C_u128;
        let mut root: Span<BytesData> = Span {
            trace_id: low,
            span_id: 1,
            name: libdd_tinybytes::BytesString::from_static("root"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        root.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("not-hex"),
        );
        let child_no_tag: Span<BytesData> = Span {
            trace_id: low,
            span_id: 2,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child_no_tag"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let mut child_valid: Span<BytesData> = Span {
            trace_id: low,
            span_id: 3,
            parent_id: 1,
            name: libdd_tinybytes::BytesString::from_static("child_valid"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        child_valid.meta.insert(
            "_dd.p.tid".into(),
            libdd_tinybytes::BytesString::from_static("dddddddddddddddd"),
        );
        let req = map_traces_to_otlp(vec![vec![root, child_no_tag, child_valid]], &resource_info);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        // The chunk-level scan skips the malformed root and picks up child_valid's tag,
        // which is then applied to every span in the chunk.
        let expected = "ddddddddddddddddd269b633813fc60c";
        assert_eq!(spans[0].trace_id, expected);
        assert_eq!(spans[1].trace_id, expected);
        assert_eq!(spans[2].trace_id, expected);
    }

    #[test]
    fn test_stats_computed_resource_attr_set_when_enabled() {
        let resource_info = OtlpResourceInfo {
            client_computed_stats: true,
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let resource_attrs = &req.resource_spans[0].resource.as_ref().unwrap().attributes;
        let kv = resource_attrs
            .iter()
            .find(|a| a.key == "_dd.stats_computed")
            .expect("_dd.stats_computed must be present when client_computed_stats=true");
        assert!(
            matches!(&kv.value, AnyValue::StringValue(s) if s == "true"),
            "_dd.stats_computed must be \"true\", got {:?}",
            kv.value
        );
    }

    #[test]
    fn test_stats_computed_resource_attr_absent_when_disabled() {
        let resource_info = OtlpResourceInfo {
            client_computed_stats: false,
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let resource_attrs = &req.resource_spans[0].resource.as_ref().unwrap().attributes;
        assert!(
            !resource_attrs.iter().any(|a| a.key == "_dd.stats_computed"),
            "_dd.stats_computed must not be emitted when client_computed_stats=false"
        );
    }

    #[test]
    fn test_empty_chunk_does_not_panic() {
        // Defensive: an empty chunk should produce no spans and not panic.
        let resource_info = OtlpResourceInfo::default();
        let empty: Vec<Vec<Span<BytesData>>> = vec![vec![]];
        let req = map_traces_to_otlp(empty, &resource_info);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert!(spans.is_empty());
    }

    #[test]
    fn test_tracestate_from_meta() {
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.meta.insert(
            "tracestate".into(),
            libdd_tinybytes::BytesString::from_static("vendor1=abc,rojo=00f067"),
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(
            otlp_span.trace_state.as_deref(),
            Some("vendor1=abc,rojo=00f067")
        );
    }

    #[test]
    fn test_meta_struct_as_bytes_value() {
        use libdd_tinybytes::Bytes;
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.meta_struct
            .insert("my_key".into(), Bytes::from(vec![1u8, 2, 3]));
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        let kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "my_key")
            .expect("my_key attribute not found");
        // Per the protobuf JSON mapping, bytes are base64-encoded.
        assert_eq!(kv["value"]["bytesValue"], "AQID");
    }

    #[test]
    fn test_operation_name_attribute() {
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("my.operation"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        let kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "operation.name")
            .expect("operation.name attribute not found");
        assert_eq!(kv["value"]["stringValue"], "my.operation");
    }

    #[test]
    fn test_span_type_attribute() {
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            r#type: libdd_tinybytes::BytesString::from_static("grpc"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        let kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "span.type")
            .expect("span.type attribute not found");
        assert_eq!(kv["value"]["stringValue"], "grpc");
    }

    #[test]
    fn test_resource_name_attribute() {
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            resource: libdd_tinybytes::BytesString::from_static("GET /api/users"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let otlp_span = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
        // resource maps to the OTLP span name
        assert_eq!(otlp_span["name"], "GET /api/users");
        // resource also maps to the resource.name attribute
        let kv = otlp_span["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "resource.name")
            .expect("resource.name attribute not found");
        assert_eq!(kv["value"]["stringValue"], "GET /api/users");
    }

    #[test]
    fn test_empty_resource_name_not_emitted() {
        // A span with no resource set should not emit a resource.name attribute.
        // In practice DD spans always have a resource, but the mapper is defensive about
        // empty fields from the wire.
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            // resource is empty (default)
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
            .as_array()
            .unwrap();
        assert!(
            !attrs.iter().any(|a| a["key"] == "resource.name"),
            "resource.name should not be emitted when resource is empty"
        );
    }

    #[test]
    fn test_per_span_service_name_attribute() {
        // When span.service differs from the resource-level service, service.name is emitted
        // as a per-span attribute so the receiver can distinguish between services in a trace.
        let resource_info = OtlpResourceInfo {
            service: "resource-svc".to_string(),
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            service: libdd_tinybytes::BytesString::from_static("span-svc"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let json = serde_json::to_value(&req).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        let kv = attrs
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["key"] == "service.name")
            .expect("service.name attribute not found");
        assert_eq!(kv["value"]["stringValue"], "span-svc");
    }

    #[test]
    fn test_unsampled_span_flags_zero() {
        // _sampling_priority_v1 = 0 means explicitly dropped; flags field must be 0.
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.metrics.insert("_sampling_priority_v1".into(), 0.0);
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(otlp_span.flags, Some(0));
    }
}
