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
/// Resource: SDK-level attributes (service.name, deployment.environment, telemetry.sdk.*,
/// runtime-id). InstrumentationScope: "datadog" (DD SDKs don't have scope; all spans use this).
/// All analogous DD span fields are mapped; meta→attributes (string), metrics→attributes
/// (int/double), links and events mapped to OTLP links and events. Status from span.error and
/// meta["error.msg"].
pub fn map_traces_to_otlp<T: TraceData>(
    trace_chunks: Vec<Vec<Span<T>>>,
    resource_info: &OtlpResourceInfo,
) -> ExportTraceServiceRequest {
    let resource = build_resource(resource_info);
    let mut all_spans: Vec<OtlpSpan> = Vec::new();
    for chunk in &trace_chunks {
        for span in chunk {
            all_spans.push(map_span(span));
        }
    }
    let scope = InstrumentationScope {
        name: Some("datadog".to_string()),
        version: None,
    };
    let scope_spans = ScopeSpans {
        scope: Some(scope),
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
    if !resource_info.git_commit_sha.is_empty() {
        attributes.push(KeyValue {
            key: "git.commit.sha".to_string(),
            value: AnyValue::StringValue(resource_info.git_commit_sha.clone()),
        });
    }
    if !resource_info.git_repository_url.is_empty() {
        attributes.push(KeyValue {
            key: "git.repository_url".to_string(),
            value: AnyValue::StringValue(resource_info.git_repository_url.clone()),
        });
    }
    Resource { attributes }
}

fn map_span<T: TraceData>(span: &Span<T>) -> OtlpSpan {
    // Reconstruct the full 128-bit trace ID. The v04/v05 wire format carries only the low 64 bits
    // in the trace_id field; when a tracer emits a 128-bit ID the high 64 bits are propagated as
    // the hex string meta tag "_dd.p.tid".
    let trace_id_high: u128 = span
        .meta
        .get("_dd.p.tid")
        .and_then(|v| u64::from_str_radix(v.borrow(), 16).ok())
        .unwrap_or(0) as u128;
    let trace_id_128 = (trace_id_high << 64) | span.trace_id;
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
    let (attributes, dropped_attributes_count) = map_attributes(span);
    let error_msg = span.meta.get("error.msg").map(|v| v.borrow().to_string());
    let status = if span.error != 0 {
        Some(Status {
            message: error_msg,
            code: json_types::status_code::ERROR,
        })
    } else {
        None
    };
    // Set flags from sampling priority: 1 = sampled/keep, 0 = dropped.
    let flags = span
        .metrics
        .get("_sampling_priority_v1")
        .map(|p| if *p >= 1.0 { 1u32 } else { 0u32 });
    let links = span.span_links.iter().map(map_span_link).collect();
    let (events, dropped_events_count) = map_span_events(&span.span_events);
    OtlpSpan {
        trace_id: trace_id_hex,
        span_id: span_id_hex,
        parent_span_id,
        name: span.name.borrow().to_string(),
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
    let trace_id_128 = (link.trace_id_high as u128) << 64 | (link.trace_id as u128);
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
            .filter_map(|(k, v)| event_attr_to_key_value(k, v))
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
) -> Option<KeyValue> {
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
    Some(KeyValue {
        key: k.borrow().to_string(),
        value,
    })
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

fn map_attributes<T: TraceData>(span: &Span<T>) -> (Vec<KeyValue>, usize) {
    let mut attrs: Vec<KeyValue> = Vec::new();
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
    let total = span.meta.len() + span.metrics.len();
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
            trace_id: 0x5B8EFFF798038103D269B633813FC60C_u128,
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
        assert_eq!(otlp_span.trace_id, "5b8efff798038103d269b633813fc60c");
        assert_eq!(otlp_span.span_id, "eee19b7ec3c1b174");
        assert_eq!(
            otlp_span.parent_span_id.as_deref(),
            Some("eee19b7ec3c1b173")
        );
        assert_eq!(otlp_span.kind, json_types::span_kind::SERVER);
        assert_eq!(otlp_span.start_time_unix_nano, "1544712660000000000");
        assert_eq!(otlp_span.end_time_unix_nano, "1544712661000000000");
        assert_eq!(
            rs.scope_spans[0].scope.as_ref().unwrap().name.as_deref(),
            Some("datadog")
        );
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
        let status = otlp_span.status.as_ref().unwrap();
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
}
