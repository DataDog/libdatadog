// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Maps Datadog trace/spans directly to the generated prost OTLP types (the IR).
//!
//! The prost `ExportTraceServiceRequest` is the single OTLP representation: from it the
//! HTTP/protobuf wire format is produced by prost encoding and the HTTP/JSON wire format by the
//! serde serializer in `json_serializer`. Attributes are built straight into prost
//! `KeyValue`/`AnyValue` in one pass — there is no intermediate value type to keep the two
//! encoders in sync because there is only one IR.

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

/// Maximum number of attributes per span; excess are dropped and counted.
pub(crate) const MAX_ATTRIBUTES_PER_SPAN: usize = 128;

/// OTLP SpanKind enum values.
mod span_kind {
    pub const UNSPECIFIED: i32 = 0;
    pub const INTERNAL: i32 = 1;
    pub const SERVER: i32 = 2;
    pub const CLIENT: i32 = 3;
    pub const PRODUCER: i32 = 4;
    pub const CONSUMER: i32 = 5;
}

/// OTLP StatusCode enum values. Public because the OTLP metrics exporter
/// (`libdd-data-pipeline`) reuses these constants.
pub mod status_code {
    pub const UNSET: i32 = 0;
    pub const ERROR: i32 = 2;
}

// ─── Scalar mapping helpers ──────────────────────────────────────────────────

/// OTLP status (code, optional message) for a span. ERROR with the error message when
/// `span.error != 0`, otherwise UNSET. A span carries at most one of `error.msg` / `error.message`
/// (`error.message` is used by all SDKs except .NET, which uses `error.msg`), so promote whichever
/// is present — under OTel-semantics `collect_span_attributes` drops both compat tags since the
/// message now lives in `Status`.
fn span_status<T: TraceData>(span: &Span<T>) -> (i32, Option<String>) {
    if span.error != 0 {
        let message = span
            .meta
            .get("error.msg")
            .or_else(|| span.meta.get("error.message"))
            .map(|v| v.borrow().to_string());
        (status_code::ERROR, message)
    } else {
        (status_code::UNSET, None)
    }
}

/// OTLP SpanKind for a span: prefer the explicit `span.kind` meta tag, else the DD span type.
fn span_kind<T: TraceData>(span: &Span<T>) -> i32 {
    span.meta
        .get("span.kind")
        .map(|v| tag_to_otlp_kind(v.borrow()))
        .unwrap_or_else(|| dd_type_to_otlp_kind(span.r#type.borrow()))
}

/// Resolve the high 64 bits of the chunk's 128-bit trace id (native field or `_dd.p.tid`).
fn chunk_trace_id_high<T: TraceData>(chunk: &[Span<T>]) -> u64 {
    chunk
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
        .unwrap_or(0)
}

/// Maps the explicit "span.kind" meta tag (set by OTEL-instrumented tracers) to an OTLP SpanKind.
fn tag_to_otlp_kind(t: &str) -> i32 {
    // Case-insensitive match without allocating: these are ASCII keywords, so
    // `eq_ignore_ascii_case` avoids the per-span `to_lowercase()` String on the encode hot
    // path.
    if t.eq_ignore_ascii_case("server") {
        span_kind::SERVER
    } else if t.eq_ignore_ascii_case("client") {
        span_kind::CLIENT
    } else if t.eq_ignore_ascii_case("producer") {
        span_kind::PRODUCER
    } else if t.eq_ignore_ascii_case("consumer") {
        span_kind::CONSUMER
    } else if t.eq_ignore_ascii_case("internal") {
        span_kind::INTERNAL
    } else {
        span_kind::UNSPECIFIED
    }
}

/// Maps the Datadog span type field (set by DD-instrumented tracers) to an OTLP SpanKind.
fn dd_type_to_otlp_kind(t: &str) -> i32 {
    // Case-insensitive match without allocating (see `tag_to_otlp_kind`).
    if t.eq_ignore_ascii_case("server")
        || t.eq_ignore_ascii_case("web")
        || t.eq_ignore_ascii_case("http")
    {
        span_kind::SERVER
    } else if t.eq_ignore_ascii_case("client") {
        span_kind::CLIENT
    } else if t.eq_ignore_ascii_case("producer") {
        span_kind::PRODUCER
    } else if t.eq_ignore_ascii_case("consumer") {
        span_kind::CONSUMER
    } else {
        span_kind::INTERNAL
    }
}

// ─── Attribute builders (straight into prost) ─────────────────────────────────

/// Wrap a prost attribute value as a `KeyValue`. `key_ref` is a profiling-signal field, set to
/// its zero default explicitly (no `..Default::default()`).
fn proto_kv(key: String, value: ProtoValue) -> ProtoKeyValue {
    ProtoKeyValue {
        key,
        value: Some(ProtoAnyValue { value: Some(value) }),
        key_ref: 0,
    }
}

/// Collect a span's OTLP attributes directly as prost `KeyValue`s plus the dropped count.
/// Per-span service.name (only when it differs from the resource service), operation.name,
/// span.type, resource.name, then meta (string), metrics (int when integral and in i64 range
/// else double), meta_struct (bytes), capped at `MAX_ATTRIBUTES_PER_SPAN`.
fn collect_span_attributes<T: TraceData>(
    span: &Span<T>,
    resource_service: &str,
    otel_trace_semantics_enabled: bool,
) -> (Vec<ProtoKeyValue>, usize) {
    // Pre-size to avoid reallocations as attributes accumulate. Upper bound is the 4 synthetic
    // attrs plus every meta/metrics/meta_struct entry, clamped to the per-span cap.
    let capacity = (4 + span.meta.len() + span.metrics.len() + span.meta_struct.len())
        .min(MAX_ATTRIBUTES_PER_SPAN);
    let mut attrs: Vec<ProtoKeyValue> = Vec::with_capacity(capacity);
    // With OTel-semantics enabled the DD-specific attributes are omitted: the four promoted tags
    // below, and the `error.*`/`span.kind` meta tags (that information lives in the OTLP Status
    // and Span.kind fields instead).
    let span_service = span.service.borrow();
    let has_per_span_service = !span_service.is_empty() && span_service != resource_service;
    if has_per_span_service && !otel_trace_semantics_enabled {
        attrs.push(proto_kv(
            "service.name".to_string(),
            ProtoValue::StringValue(span_service.to_string()),
        ));
    }
    let operation_name = span.name.borrow();
    let has_operation_name = !operation_name.is_empty();
    if has_operation_name && !otel_trace_semantics_enabled {
        attrs.push(proto_kv(
            "operation.name".to_string(),
            ProtoValue::StringValue(operation_name.to_string()),
        ));
    }
    let span_type = span.r#type.borrow();
    let has_span_type = !span_type.is_empty();
    if has_span_type && !otel_trace_semantics_enabled {
        attrs.push(proto_kv(
            "span.type".to_string(),
            ProtoValue::StringValue(span_type.to_string()),
        ));
    }
    let resource_name = span.resource.borrow();
    let has_resource_name = !resource_name.is_empty();
    if has_resource_name && !otel_trace_semantics_enabled {
        attrs.push(proto_kv(
            "resource.name".to_string(),
            ProtoValue::StringValue(resource_name.to_string()),
        ));
    }
    for (k, v) in span.meta.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        let key = k.borrow();
        if otel_trace_semantics_enabled
            && (key == "error.msg" || key == "error.message" || key == "span.kind")
        {
            continue;
        }
        attrs.push(proto_kv(
            key.to_string(),
            ProtoValue::StringValue(v.borrow().to_string()),
        ));
    }
    for (k, v) in span.metrics.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        let value = if v.fract() == 0.0 && (*v >= i64::MIN as f64 && *v <= i64::MAX as f64) {
            ProtoValue::IntValue(*v as i64)
        } else {
            ProtoValue::DoubleValue(*v)
        };
        attrs.push(proto_kv(k.borrow().to_string(), value));
    }
    for (k, v) in span.meta_struct.iter() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        attrs.push(proto_kv(
            k.borrow().to_string(),
            ProtoValue::BytesValue(v.borrow().to_vec()),
        ));
    }
    // Dropped-count accounting must mirror what was actually emitted: with OTel-semantics on, the
    // promoted tags aren't added and the excluded `error.*`/`span.kind` meta tags drop out of the
    // meta total.
    let excluded_compat_tags = if otel_trace_semantics_enabled {
        span.meta.contains_key("error.msg") as usize
            + span.meta.contains_key("error.message") as usize
            + span.meta.contains_key("span.kind") as usize
    } else {
        0
    };
    let promoted = if otel_trace_semantics_enabled {
        0
    } else {
        (has_per_span_service as usize)
            + (has_operation_name as usize)
            + (has_span_type as usize)
            + (has_resource_name as usize)
    };
    let total = promoted
        + (span.meta.len() - excluded_compat_tags)
        + span.metrics.len()
        + span.meta_struct.len();
    let dropped = total.saturating_sub(attrs.len());
    (attrs, dropped)
}

/// A single event/link attribute value → prost (events carry typed single/array values).
fn event_attr_value<T: TraceData>(av: &crate::span::v04::AttributeArrayValue<T>) -> ProtoValue {
    use crate::span::v04::AttributeArrayValue;
    match av {
        AttributeArrayValue::String(s) => ProtoValue::StringValue(s.borrow().to_string()),
        AttributeArrayValue::Boolean(b) => ProtoValue::BoolValue(*b),
        AttributeArrayValue::Integer(i) => ProtoValue::IntValue(*i),
        AttributeArrayValue::Double(d) => ProtoValue::DoubleValue(*d),
    }
}

fn collect_event_attributes<T: TraceData>(ev: &SpanEvent<T>) -> Vec<ProtoKeyValue> {
    use crate::span::v04::AttributeAnyValue;
    ev.attributes
        .iter()
        .map(|(k, v)| {
            let value = match v {
                AttributeAnyValue::SingleValue(av) => event_attr_value(av),
                AttributeAnyValue::Array(items) => ProtoValue::ArrayValue(ProtoArrayValue {
                    values: items
                        .iter()
                        .map(|it| ProtoAnyValue {
                            value: Some(event_attr_value(it)),
                        })
                        .collect(),
                }),
            };
            proto_kv(k.borrow().to_string(), value)
        })
        .collect()
}

// ─── Public mapper ────────────────────────────────────────────────────────────

/// Maps Datadog trace chunks and resource info to a prost OTLP `ExportTraceServiceRequest`, built
/// directly from the native span fields (no hex/decimal round trip — the prost types are the IR).
///
/// Resource: SDK-level attributes (service.name, deployment.environment.name, telemetry.sdk.*,
/// runtime-id). InstrumentationScope: optional tracer scope name/version.
/// All analogous DD span fields are mapped; meta→attributes (string), metrics→attributes
/// (int/double), links and events mapped to OTLP links and events. Status from span.error and
/// meta["error.msg"] or meta["error.message"].
///
/// The high 64 bits of a 128-bit trace ID are carried in the trace_id field itself or (if not
/// present) as the `_dd.p.tid` meta tag, which per RFC #85 is set on the chunk root only.
/// We resolve it once per chunk and apply it to every span so OTLP receivers see the full 128-bit
/// trace_id on every span in the trace.
pub fn map_traces_to_otlp<T: TraceData>(
    trace_chunks: Vec<Vec<Span<T>>>,
    resource_info: &OtlpResourceInfo,
    otel_trace_semantics_enabled: bool,
) -> ProtoReq {
    let resource = build_resource(resource_info);
    // Pre-size to the total span count so the per-span push loop never reallocates.
    let total_spans: usize = trace_chunks.iter().map(|chunk| chunk.len()).sum();
    let mut all_spans: Vec<ProtoSpan> = Vec::with_capacity(total_spans);
    for chunk in &trace_chunks {
        // Resolve the high 64 bits of the 128-bit trace ID once per chunk. For each span,
        // prefer the native u128 `trace_id` field (e.g. Python's native spans hold the full
        // 128-bit ID there) and fall back to its RFC #85 `_dd.p.tid` meta tag.
        let high = chunk_trace_id_high(chunk);
        for span in chunk {
            all_spans.push(map_span(
                span,
                &resource_info.service,
                high,
                otel_trace_semantics_enabled,
            ));
        }
    }
    ProtoReq {
        resource_spans: vec![ProtoResourceSpans {
            resource: Some(resource),
            scope_spans: vec![ProtoScopeSpans {
                scope: Some(ProtoScope {
                    name: resource_info.instrumentation_scope_name.clone(),
                    version: resource_info.instrumentation_scope_version.clone(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                spans: all_spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

fn build_resource(resource_info: &OtlpResourceInfo) -> ProtoResource {
    fn push_str_attr(attrs: &mut Vec<ProtoKeyValue>, k: &str, v: &str) {
        if !v.is_empty() {
            attrs.push(proto_kv(
                k.to_string(),
                ProtoValue::StringValue(v.to_string()),
            ));
        }
    }
    let mut attributes = Vec::new();
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
    attributes.push(proto_kv(
        "telemetry.sdk.name".to_string(),
        ProtoValue::StringValue("datadog".to_string()),
    ));
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
    // Tells Datadog Agent OTLP receivers to skip their concentrator; prevents double-counted
    // APM metrics.
    if resource_info.client_computed_stats {
        push_str_attr(&mut attributes, "_dd.stats_computed", "true");
    }
    // `entity_refs` is a profiling-signal-only field; explicit default.
    ProtoResource {
        attributes,
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
    }
}

fn map_span<T: TraceData>(
    span: &Span<T>,
    resource_service: &str,
    chunk_trace_id_high: u64,
    otel_trace_semantics_enabled: bool,
) -> ProtoSpan {
    // Reconstruct the full 128-bit trace ID. The caller resolves the high 64 bits once per
    // chunk (from either the native u128 `trace_id` field or the "_dd.p.tid" meta tag).
    // All spans in a chunk share the same trace ID.
    let trace_id_128 = ((chunk_trace_id_high as u128) << 64) | (span.trace_id as u64 as u128);
    let parent_span_id = if span.parent_id != 0 {
        span.parent_id.to_be_bytes().to_vec()
    } else {
        Vec::new()
    };
    let (attributes, dropped_attributes_count) =
        collect_span_attributes(span, resource_service, otel_trace_semantics_enabled);
    let (code, message) = span_status(span);
    let flags = span
        .metrics
        .get("_sampling_priority_v1")
        .map(|p| (*p >= 1.0) as u32)
        .unwrap_or(0);
    let trace_state = span
        .meta
        .get("tracestate")
        .map(|v| v.borrow().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let links = span.span_links.iter().map(map_span_link).collect();
    let (events, dropped_events_count) = map_span_events(&span.span_events);
    ProtoSpan {
        trace_id: trace_id_128.to_be_bytes().to_vec(),
        span_id: span.span_id.to_be_bytes().to_vec(),
        trace_state,
        parent_span_id,
        flags,
        name: span.resource.borrow().to_string(),
        kind: span_kind(span),
        // OTLP timestamps are unsigned; clamp negatives to 0 so the `as u64` cast can't wrap.
        start_time_unix_nano: span.start.max(0) as u64,
        end_time_unix_nano: (span.start + span.duration).max(0) as u64,
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

fn map_span_link<T: TraceData>(link: &SpanLink<T>) -> ProtoLink {
    let trace_id_128 = ((link.trace_id_high as u128) << 64) | (link.trace_id as u128);
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
        attributes: link
            .attributes
            .iter()
            .map(|(k, v)| {
                proto_kv(
                    k.borrow().to_string(),
                    ProtoValue::StringValue(v.borrow().to_string()),
                )
            })
            .collect(),
        dropped_attributes_count: 0,
        // W3C trace flags of the linked context (sampled bit, etc.); carry them through so OTLP
        // consumers see the same link metadata the tracer recorded.
        flags: link.flags,
    }
}

fn map_span_events<T: TraceData>(events: &[SpanEvent<T>]) -> (Vec<ProtoEvent>, usize) {
    const MAX_EVENTS_PER_SPAN: usize = 128;
    let mut out = Vec::with_capacity(events.len().min(MAX_EVENTS_PER_SPAN));
    for ev in events.iter().take(MAX_EVENTS_PER_SPAN) {
        out.push(ProtoEvent {
            time_unix_nano: ev.time_unix_nano,
            name: ev.name.borrow().to_string(),
            attributes: collect_event_attributes(ev),
            dropped_attributes_count: 0,
        });
    }
    let dropped = events.len().saturating_sub(out.len());
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otlp_encoder::OtlpResourceInfo;
    use crate::span::BytesData;

    #[test]
    fn maps_native_span_to_prost_ir() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 0xD269B633813FC60C_u128,
            span_id: 0xEEE19B7EC3C1B174,
            parent_id: 0xEEE19B7EC3C1B173,
            name: libdd_tinybytes::BytesString::from_static("op"),
            resource: libdd_tinybytes::BytesString::from_static("res"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 1544712660000000000,
            duration: 1000000000,
            error: 1,
            ..Default::default()
        };
        span.meta.insert(
            "error.msg".into(),
            libdd_tinybytes::BytesString::from_static("boom"),
        );
        span.metrics
            .insert(libdd_tinybytes::BytesString::from_static("count"), 42.0);
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.trace_id, 0xD269B633813FC60C_u128.to_be_bytes().to_vec());
        assert_eq!(s.span_id, 0xEEE19B7EC3C1B174u64.to_be_bytes().to_vec());
        assert_eq!(
            s.parent_span_id,
            0xEEE19B7EC3C1B173u64.to_be_bytes().to_vec()
        );
        assert_eq!(s.name, "res");
        assert_eq!(s.kind, 2); // SERVER (from dd type "web")
        assert_eq!(s.start_time_unix_nano, 1544712660000000000);
        assert_eq!(s.end_time_unix_nano, 1544712661000000000);
        let st = s.status.as_ref().unwrap();
        assert_eq!(st.code, 2);
        assert_eq!(st.message, "boom");
        let count = s.attributes.iter().find(|a| a.key == "count").unwrap();
        assert!(matches!(
            count.value.as_ref().unwrap().value,
            Some(PV::IntValue(42))
        ));
    }

    #[test]
    fn instrumentation_scope_from_resource_info() {
        let resource_info = OtlpResourceInfo {
            instrumentation_scope_name: "dd-trace-js".to_string(),
            instrumentation_scope_version: "7.0.0-pre".to_string(),
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

        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let scope = req.resource_spans[0].scope_spans[0].scope.as_ref().unwrap();
        assert_eq!(scope.name, "dd-trace-js");
        assert_eq!(scope.version, "7.0.0-pre");
    }

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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
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
        assert_eq!(s.kind, span_kind::SERVER);
    }

    #[test]
    fn negative_start_clamps_to_zero() {
        // Regression test: a span with negative start (malformed input) must map to
        // start_time_unix_nano == 0 (and not wrap to u64::MAX), matching the old parse_u64
        // behavior.
        let resource_info = OtlpResourceInfo {
            service: "svc".to_string(),
            ..Default::default()
        };
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 1,
            start: -1,
            duration: 0,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(
            s.start_time_unix_nano, 0,
            "negative start must clamp to 0, not wrap"
        );
        assert_eq!(
            s.end_time_unix_nano, 0,
            "negative start+duration must clamp to 0, not wrap"
        );
    }

    #[test]
    fn status_error_message_from_meta() {
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let status = s.status.as_ref().unwrap();
        assert_eq!(status.code, status_code::ERROR);
        assert_eq!(status.message, "something broke");
    }

    #[test]
    fn metrics_as_int_or_double() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let count = s.attributes.iter().find(|a| a.key == "count").unwrap();
        assert!(matches!(
            count.value.as_ref().unwrap().value,
            Some(PV::IntValue(42))
        ));
        let rate = s.attributes.iter().find(|a| a.key == "rate").unwrap();
        match rate.value.as_ref().unwrap().value {
            Some(PV::DoubleValue(d)) => assert!((d - std::f64::consts::PI).abs() < 1e-9),
            ref other => panic!("expected double, got {other:?}"),
        }
    }

    #[test]
    fn trace_id_128_from_dd_p_tid() {
        // When "_dd.p.tid" is present it supplies the high 64 bits of the trace ID.
        // Low 64 bits come from span.trace_id; the two are concatenated to form a 128-bit ID.
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(
            s.trace_id,
            0x5b8efff798038103_d269b633813fc60c_u128
                .to_be_bytes()
                .to_vec()
        );
    }

    #[test]
    fn trace_id_128_from_native_span_field() {
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
        let req = map_traces_to_otlp(vec![vec![root, child]], &resource_info, false);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        let expected = full.to_be_bytes().to_vec();
        assert_eq!(spans[0].trace_id, expected);
        assert_eq!(spans[1].trace_id, expected);
    }

    #[test]
    fn trace_id_128_without_dd_p_tid_defaults_high_to_zero() {
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.trace_id, 0xD269B633813FC60C_u128.to_be_bytes().to_vec());
    }

    #[test]
    fn trace_id_128_propagated_to_chunk_children() {
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
        let req = map_traces_to_otlp(vec![vec![root, child_a, child_b]], &resource_info, false);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 3);
        let expected = 0x5b8efff798038103_d269b633813fc60c_u128
            .to_be_bytes()
            .to_vec();
        for s in spans {
            assert_eq!(s.trace_id, expected);
        }
    }

    #[test]
    fn trace_id_128_isolation_across_chunks() {
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
            false,
        );
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans.len(), 4);
        let expect_a = 0xaaaaaaaaaaaaaaaa_1111111111111111_u128
            .to_be_bytes()
            .to_vec();
        let expect_b = 0xbbbbbbbbbbbbbbbb_2222222222222222_u128
            .to_be_bytes()
            .to_vec();
        assert_eq!(spans[0].trace_id, expect_a);
        assert_eq!(spans[1].trace_id, expect_a);
        assert_eq!(spans[2].trace_id, expect_b);
        assert_eq!(spans[3].trace_id, expect_b);
    }

    #[test]
    fn chunk_with_malformed_dd_p_tid_on_root_falls_back() {
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
        let req = map_traces_to_otlp(
            vec![vec![root, child_no_tag, child_valid]],
            &resource_info,
            false,
        );
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        // The chunk-level scan skips the malformed root and picks up child_valid's tag,
        // which is then applied to every span in the chunk.
        let expected = 0xdddddddddddddddd_d269b633813fc60c_u128
            .to_be_bytes()
            .to_vec();
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let resource_attrs = &req.resource_spans[0].resource.as_ref().unwrap().attributes;
        let kv = resource_attrs
            .iter()
            .find(|a| a.key == "_dd.stats_computed")
            .expect("_dd.stats_computed must be present when client_computed_stats=true");
        let val = match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(ProtoValue::StringValue(s)) => s.as_str(),
            other => panic!("expected stringValue, got {other:?}"),
        };
        assert_eq!(val, "true");
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let resource_attrs = &req.resource_spans[0].resource.as_ref().unwrap().attributes;
        assert!(
            !resource_attrs.iter().any(|a| a.key == "_dd.stats_computed"),
            "_dd.stats_computed must not be emitted when client_computed_stats=false"
        );
    }

    #[test]
    fn span_link_flags_are_carried() {
        // Regression: the mapper previously hardcoded `flags: 0`, dropping the linked context's
        // W3C trace flags. They must survive into the OTLP `Link.flags` field.
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("s"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.span_links.push(SpanLink {
            trace_id: 0x11,
            span_id: 0x22,
            flags: 1,
            ..Default::default()
        });
        let req = map_traces_to_otlp(vec![vec![span]], &OtlpResourceInfo::default(), false);
        let link = &req.resource_spans[0].scope_spans[0].spans[0].links[0];
        assert_eq!(
            link.flags, 1,
            "OTLP Link.flags must carry the span link's flags"
        );
    }

    #[test]
    fn test_otel_trace_semantics_enabled() {
        // With OTel-semantics on, the DD-promoted attributes (service.name/operation.name/
        // resource.name/span.type) and the error.*/span.kind meta tags are omitted; other
        // (OTel-standard) meta tags remain.
        let resource_info = OtlpResourceInfo {
            service: "resource-svc".to_string(),
            ..Default::default()
        };
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("http.request"),
            service: libdd_tinybytes::BytesString::from_static("span-svc"),
            resource: libdd_tinybytes::BytesString::from_static("GET /api/users"),
            r#type: libdd_tinybytes::BytesString::from_static("web"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        span.meta.insert(
            libdd_tinybytes::BytesString::from_static("span.kind"),
            libdd_tinybytes::BytesString::from_static("client"),
        );
        span.meta.insert(
            libdd_tinybytes::BytesString::from_static("http.method"),
            libdd_tinybytes::BytesString::from_static("GET"),
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, true);
        let attrs = &req.resource_spans[0].scope_spans[0].spans[0].attributes;
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        for omitted in [
            "service.name",
            "operation.name",
            "resource.name",
            "span.type",
            "span.kind",
        ] {
            assert!(
                !keys.contains(&omitted),
                "OTel-semantics must omit {omitted}"
            );
        }
        assert!(
            keys.contains(&"http.method"),
            "OTel-standard meta tags must remain"
        );
    }

    #[test]
    fn error_message_promoted_to_status_under_otel_semantics() {
        // Regression: `error.message` (used by every SDK except .NET) must be promoted to the OTLP
        // Status message even when OTel-semantics drops the compat meta tag — otherwise the error
        // text is lost entirely (neither in Status nor in the attributes).
        let resource_info = OtlpResourceInfo::default();
        let mut span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("op"),
            start: 0,
            duration: 1,
            error: 1,
            ..Default::default()
        };
        span.meta.insert(
            libdd_tinybytes::BytesString::from_static("error.message"),
            libdd_tinybytes::BytesString::from_static("boom"),
        );
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, true);
        let otlp_span = &req.resource_spans[0].scope_spans[0].spans[0];
        let status = otlp_span
            .status
            .as_ref()
            .expect("status present on error span");
        assert_eq!(status.code, status_code::ERROR);
        assert_eq!(
            status.message, "boom",
            "error.message must be promoted to the OTLP Status message"
        );
        assert!(
            !otlp_span
                .attributes
                .iter()
                .any(|kv| kv.key == "error.message"),
            "error.message compat attr must be omitted under OTel-semantics"
        );
    }

    #[test]
    fn empty_chunk_does_not_panic() {
        // Defensive: an empty chunk should produce no spans and not panic.
        let resource_info = OtlpResourceInfo::default();
        let empty: Vec<Vec<Span<BytesData>>> = vec![vec![]];
        let req = map_traces_to_otlp(empty, &resource_info, false);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert!(spans.is_empty());
    }

    #[test]
    fn tracestate_from_meta() {
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.trace_state, "vendor1=abc,rojo=00f067");
    }

    #[test]
    fn meta_struct_as_bytes_value() {
        use libdd_tinybytes::Bytes;
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "my_key")
            .expect("my_key attribute not found");
        match kv.value.as_ref().unwrap().value {
            Some(PV::BytesValue(ref b)) => assert_eq!(b, &vec![1u8, 2, 3]),
            ref other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn operation_name_attribute() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
        let resource_info = OtlpResourceInfo::default();
        let span: Span<BytesData> = Span {
            trace_id: 1,
            span_id: 2,
            name: libdd_tinybytes::BytesString::from_static("my.operation"),
            start: 0,
            duration: 1,
            ..Default::default()
        };
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "operation.name")
            .expect("operation.name attribute not found");
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "my.operation"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn span_type_attribute() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "span.type")
            .expect("span.type attribute not found");
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "grpc"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn resource_name_attribute_and_span_name() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        // resource maps to the OTLP span name
        assert_eq!(s.name, "GET /api/users");
        // resource also maps to the resource.name attribute
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "resource.name")
            .expect("resource.name attribute not found");
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "GET /api/users"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn empty_resource_name_not_emitted() {
        // A span with no resource set should not emit a resource.name attribute.
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert!(
            !s.attributes.iter().any(|a| a.key == "resource.name"),
            "resource.name should not be emitted when resource is empty"
        );
    }

    #[test]
    fn per_span_service_name_attribute() {
        use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "service.name")
            .expect("service.name attribute not found");
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "span-svc"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn unsampled_span_flags_zero() {
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
        let req = map_traces_to_otlp(vec![vec![span]], &resource_info, false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.flags, 0);
    }
}
