// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! v1-native analog of [`super::mapper::map_traces_to_otlp`]: maps `v1::TraceChunk`/`v1::Span`
//! directly to the prost OTLP IR.
//!
//! Unlike the v0.4 mapper, this needs no hex/decimal round trip *and* no meta/metrics flatten:
//! v1's `trace_id: [u8; 16]` is already the full 128-bit id, `span_kind: SpanKind` already carries
//! the OTLP span kind, sampling priority lives on the chunk, and `AttributeValue` maps onto OTLP's
//! `AnyValue` losslessly (string/bool/int/double/bytes/array/kvlist), so attributes are converted
//! recursively with no flattening or type coercion.

use super::mapper::{build_resource, proto_kv, MAX_ATTRIBUTES_PER_SPAN};
use super::OtlpResourceInfo;
use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanLink, TraceChunk};
use crate::span::{TraceData, SPAN_LINK_FLAGS_SET_SENTINEL};
use std::borrow::Borrow;

use libdd_trace_protobuf::opentelemetry::proto::collector::trace::v1::ExportTraceServiceRequest as ProtoReq;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    any_value::Value as ProtoValue, AnyValue as ProtoAnyValue, ArrayValue as ProtoArrayValue,
    InstrumentationScope as ProtoScope, KeyValue as ProtoKeyValue, KeyValueList as ProtoKvList,
};
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{
    span::{Event as ProtoEvent, Link as ProtoLink},
    ResourceSpans as ProtoResourceSpans, ScopeSpans as ProtoScopeSpans, Span as ProtoSpan,
    Status as ProtoStatus,
};

use super::mapper::status_code;

/// Meta keys promoted to a dedicated OTLP location (Status message), so they're excluded from
/// the emitted attributes under OTel-semantics — mirrors the v0.4 mapper's `error.msg`/
/// `error.message` handling.
const ERROR_MESSAGE_KEYS: [&str; 2] = ["error.msg", "error.message"];

/// Converts a v1 `AttributeValue` directly to a prost `AnyValue::Value`. Lossless: every variant
/// (including nested `List`/`KeyValue`) maps onto an OTLP-native shape, unlike the v0.4 downgrade
/// encoders which have to flatten into a meta/metrics string/f64 split.
fn attr_value_to_proto<T: TraceData>(value: &AttributeValue<T>) -> ProtoValue {
    match value {
        AttributeValue::String(s) => ProtoValue::StringValue(s.borrow().to_string()),
        AttributeValue::Bool(b) => ProtoValue::BoolValue(*b),
        AttributeValue::Int(i) => ProtoValue::IntValue(*i),
        AttributeValue::Float(f) => ProtoValue::DoubleValue(*f),
        AttributeValue::Bytes(b) => ProtoValue::BytesValue(b.borrow().to_vec()),
        AttributeValue::List(items) => ProtoValue::ArrayValue(ProtoArrayValue {
            values: items
                .iter()
                .map(|it| ProtoAnyValue {
                    value: Some(attr_value_to_proto(it)),
                })
                .collect(),
        }),
        AttributeValue::KeyValue(map) => ProtoValue::KvlistValue(ProtoKvList {
            values: map
                .defensive_dedup()
                .iter()
                .map(|(k, v)| proto_kv(k.borrow().to_string(), attr_value_to_proto(v)))
                .collect(),
        }),
    }
}

/// Merge chunk-level and span-level attributes into one ordered, deduplicated key/value list,
/// span wins on key collision. Both `VecMap`s are deduplicated first (last value wins).
fn merged_attrs_v1<'a, T: TraceData>(
    chunk: &'a TraceChunk<T>,
    span: &'a Span<T>,
) -> Vec<(&'a str, &'a AttributeValue<T>)> {
    let mut order: Vec<&'a str> =
        Vec::with_capacity(chunk.attributes.len() + span.attributes.len());
    let mut merged: std::collections::HashMap<&'a str, &'a AttributeValue<T>> =
        std::collections::HashMap::with_capacity(order.capacity());
    for (k, v) in chunk.attributes.iter() {
        let key = (*k).borrow();
        if merged.insert(key, v).is_none() {
            order.push(key);
        }
    }
    for (k, v) in span.attributes.iter() {
        let key = (*k).borrow();
        if merged.insert(key, v).is_none() {
            order.push(key);
        }
    }
    order.into_iter().map(|k| (k, merged[k])).collect()
}

/// OTLP status (code, optional message) for a span: ERROR with the error message when
/// `span.error` is set, otherwise UNSET. See [`super::mapper::span_status`] for the v0.4
/// equivalent and the `error.msg`/`error.message` rationale.
fn span_status_v1<T: TraceData>(
    span: &Span<T>,
    merged: &[(&str, &AttributeValue<T>)],
) -> (i32, Option<String>) {
    if !span.error {
        return (status_code::UNSET, None);
    }
    let message = ERROR_MESSAGE_KEYS.iter().find_map(|key| {
        merged.iter().find(|(k, _)| k == key).and_then(|(_, v)| {
            if let AttributeValue::String(s) = v {
                Some(s.borrow().to_string())
            } else {
                None
            }
        })
    });
    (status_code::ERROR, message)
}

/// Collect a span's OTLP attributes directly as prost `KeyValue`s plus the dropped count. Per-span
/// service.name (only when it differs from the resource service), operation.name, span.type,
/// resource.name, then the merged chunk+span attributes (recursively converted, no flattening),
/// capped at `MAX_ATTRIBUTES_PER_SPAN`.
fn collect_span_attributes_v1<T: TraceData>(
    span: &Span<T>,
    merged: &[(&str, &AttributeValue<T>)],
    resource_service: &str,
    otel_trace_semantics_enabled: bool,
) -> (Vec<ProtoKeyValue>, usize) {
    // Pre-size to avoid reallocations as attributes accumulate. Upper bound is the 4 synthetic
    // attrs plus every merged chunk+span attribute, clamped to the per-span cap.
    let capacity = (4 + merged.len()).min(MAX_ATTRIBUTES_PER_SPAN);
    let mut attrs: Vec<ProtoKeyValue> = Vec::with_capacity(capacity);
    // With OTel-semantics enabled the DD-specific attributes are omitted: the four promoted tags
    // below, and the `error.msg`/`error.message` compat tags (that information lives in the OTLP
    // Status field instead).
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
    let mut excluded_compat_tags = 0usize;
    for (key, value) in merged {
        if attrs.len() >= MAX_ATTRIBUTES_PER_SPAN {
            break;
        }
        if otel_trace_semantics_enabled && ERROR_MESSAGE_KEYS.contains(key) {
            excluded_compat_tags += 1;
            continue;
        }
        attrs.push(proto_kv(key.to_string(), attr_value_to_proto(value)));
    }
    // Dropped-count accounting must mirror what was actually emitted: with OTel-semantics on, the
    // promoted tags aren't added and the excluded `error.msg`/`error.message` tags drop out of
    // the merged total.
    let promoted = if otel_trace_semantics_enabled {
        0
    } else {
        (has_per_span_service as usize)
            + (has_operation_name as usize)
            + (has_span_type as usize)
            + (has_resource_name as usize)
    };
    let total = promoted + merged.len() - excluded_compat_tags;
    let dropped = total.saturating_sub(attrs.len());
    (attrs, dropped)
}

fn map_span_link_v1<T: TraceData>(link: &SpanLink<T>) -> ProtoLink {
    ProtoLink {
        trace_id: link.trace_id.to_vec(),
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
            .defensive_dedup()
            .iter()
            .map(|(k, v)| proto_kv(k.borrow().to_string(), attr_value_to_proto(v)))
            .collect(),
        dropped_attributes_count: 0,
        // See `super::mapper::map_span_link`: bit 31 is an internal "explicitly set" sentinel
        // that must not leak into OTLP's flags field.
        flags: link.flags & !SPAN_LINK_FLAGS_SET_SENTINEL,
    }
}

fn map_span_events_v1<T: TraceData>(events: &[SpanEvent<T>]) -> (Vec<ProtoEvent>, usize) {
    const MAX_EVENTS_PER_SPAN: usize = 128;
    let mut out = Vec::with_capacity(events.len().min(MAX_EVENTS_PER_SPAN));
    for ev in events.iter().take(MAX_EVENTS_PER_SPAN) {
        out.push(ProtoEvent {
            time_unix_nano: ev.time_unix_nano,
            name: ev.name.borrow().to_string(),
            attributes: ev
                .attributes
                .defensive_dedup()
                .iter()
                .map(|(k, v)| proto_kv(k.borrow().to_string(), attr_value_to_proto(v)))
                .collect(),
            dropped_attributes_count: 0,
        });
    }
    let dropped = events.len().saturating_sub(out.len());
    (out, dropped)
}

fn map_span_v1<T: TraceData>(
    span: &Span<T>,
    chunk: &TraceChunk<T>,
    resource_service: &str,
    flags: u32,
    otel_trace_semantics_enabled: bool,
) -> ProtoSpan {
    let parent_span_id = if span.parent_id != 0 {
        span.parent_id.to_be_bytes().to_vec()
    } else {
        Vec::new()
    };
    let merged = merged_attrs_v1(chunk, span);
    let (attributes, dropped_attributes_count) = collect_span_attributes_v1(
        span,
        &merged,
        resource_service,
        otel_trace_semantics_enabled,
    );
    let (code, message) = span_status_v1(span, &merged);
    let trace_state = merged
        .iter()
        .find(|(k, _)| *k == "tracestate")
        .and_then(|(_, v)| {
            if let AttributeValue::String(s) = v {
                Some(s.borrow().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();
    let links = span.span_links.iter().map(map_span_link_v1).collect();
    let (events, dropped_events_count) = map_span_events_v1(&span.span_events);
    ProtoSpan {
        trace_id: chunk.trace_id.to_vec(),
        span_id: span.span_id.to_be_bytes().to_vec(),
        trace_state,
        parent_span_id,
        flags,
        name: span.resource.borrow().to_string(),
        // v1's `span_kind` discriminants (Internal=1, Server=2, Client=3, Producer=4,
        // Consumer=5) already match the OTLP SpanKind enum, so no meta lookup/fallback is
        // needed like the v0.4 mapper's `span_kind()`/`tag_to_otlp_kind`/`dd_type_to_otlp_kind`.
        kind: span.span_kind as i32,
        // OTLP timestamps are unsigned; clamp negatives to 0 so the `as u64` cast can't wrap.
        start_time_unix_nano: span.start.max(0) as u64,
        end_time_unix_nano: span.start.saturating_add(span.duration).max(0) as u64,
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

/// v1-native analog of [`super::mapper::map_traces_to_otlp`]: maps v1 trace chunks and resource
/// info to a prost OTLP `ExportTraceServiceRequest`.
///
/// `chunk.trace_id` is already the full 128-bit id (no `_dd.p.tid` reconstruction needed), and
/// sampling priority lives on the chunk (`chunk.priority`) rather than a per-span metric, so it's
/// resolved once per chunk and applied to every span's `flags`, matching the RFC #85 "trace-level
/// priority" semantics that v0.4 emulates per-span via `_sampling_priority_v1`.
pub fn map_traces_to_otlp_v1<T: TraceData>(
    trace_chunks: &[TraceChunk<T>],
    resource_info: &OtlpResourceInfo,
    otel_trace_semantics_enabled: bool,
) -> ProtoReq {
    let resource = build_resource(resource_info);
    // Pre-size to the total span count so the per-span push loop never reallocates.
    let total_spans: usize = trace_chunks.iter().map(|chunk| chunk.spans.len()).sum();
    let mut all_spans: Vec<ProtoSpan> = Vec::with_capacity(total_spans);
    for chunk in trace_chunks {
        // Resolve the chunk-level sampling priority once per chunk and apply it to every span's
        // flags (see the module/function doc for why this differs from v0.4's per-span metric).
        let flags = chunk.priority.map(|p| (p >= 1) as u32).unwrap_or(0);
        for span in &chunk.spans {
            all_spans.push(map_span_v1(
                span,
                chunk,
                &resource_info.service,
                flags,
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

#[cfg(test)]
mod tests_v1 {
    use super::*;
    use crate::span::v1::SpanKind;
    use crate::span::BytesData;
    use libdd_tinybytes::BytesString;
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value as PV;

    fn bs(s: &str) -> BytesString {
        BytesString::from_string(s.to_string())
    }

    fn minimal_span() -> Span<BytesData> {
        Span {
            span_id: 2,
            name: bs("op"),
            resource: bs("res"),
            start: 1544712660000000000,
            duration: 1000000000,
            ..Default::default()
        }
    }

    fn minimal_chunk(trace_id: [u8; 16], span: Span<BytesData>) -> TraceChunk<BytesData> {
        TraceChunk {
            trace_id,
            spans: vec![span],
            ..Default::default()
        }
    }

    #[test]
    fn maps_trace_id_directly_from_chunk() {
        let chunk = minimal_chunk([0xAB; 16], minimal_span());
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.trace_id, vec![0xAB; 16]);
    }

    #[test]
    fn span_kind_maps_directly_from_dedicated_field() {
        let mut span = minimal_span();
        span.span_kind = SpanKind::Consumer;
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.kind, 5); // OTLP SpanKind CONSUMER
    }

    #[test]
    fn error_true_promotes_error_message_attribute_to_status() {
        let mut span = minimal_span();
        span.error = true;
        span.attributes
            .insert(bs("error.message"), AttributeValue::String(bs("boom")));
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let status = s.status.as_ref().unwrap();
        assert_eq!(status.code, status_code::ERROR);
        assert_eq!(status.message, "boom");
    }

    #[test]
    fn error_false_yields_unset_status() {
        let chunk = minimal_chunk([1; 16], minimal_span());
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.status.as_ref().unwrap().code, status_code::UNSET);
    }

    #[test]
    fn chunk_priority_sets_flags_for_every_span() {
        let mut chunk = minimal_chunk([1; 16], minimal_span());
        chunk.spans.push(Span {
            span_id: 3,
            parent_id: 2,
            name: bs("child"),
            resource: bs("res2"),
            start: 0,
            duration: 1,
            ..Default::default()
        });
        chunk.priority = Some(2);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let spans = &req.resource_spans[0].scope_spans[0].spans;
        assert_eq!(spans[0].flags, 1);
        assert_eq!(spans[1].flags, 1);
    }

    #[test]
    fn negative_priority_yields_zero_flags() {
        let mut chunk = minimal_chunk([1; 16], minimal_span());
        chunk.priority = Some(-1);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.flags, 0);
    }

    #[test]
    fn no_priority_yields_zero_flags() {
        let chunk = minimal_chunk([1; 16], minimal_span());
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.flags, 0);
    }

    #[test]
    fn span_attribute_overrides_chunk_attribute_of_same_key() {
        let mut chunk = minimal_chunk([1; 16], minimal_span());
        chunk
            .attributes
            .insert(bs("k"), AttributeValue::String(bs("chunk-value")));
        chunk.spans[0]
            .attributes
            .insert(bs("k"), AttributeValue::String(bs("span-value")));
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s.attributes.iter().find(|a| a.key == "k").unwrap();
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "span-value"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn chunk_attribute_propagates_to_span_without_override() {
        let mut chunk = minimal_chunk([1; 16], minimal_span());
        chunk
            .attributes
            .insert(bs("env"), AttributeValue::String(bs("prod")));
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s.attributes.iter().find(|a| a.key == "env").unwrap();
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "prod"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn int_and_float_and_bool_and_bytes_attributes_map_typed() {
        let mut span = minimal_span();
        span.attributes.insert(bs("i"), AttributeValue::Int(42));
        span.attributes
            .insert(bs("f"), AttributeValue::Float(std::f64::consts::PI));
        span.attributes.insert(bs("b"), AttributeValue::Bool(true));
        span.attributes
            .insert(bs("by"), AttributeValue::Bytes(vec![1, 2, 3].into()));
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let get = |k: &str| {
            s.attributes
                .iter()
                .find(|a| a.key == k)
                .unwrap()
                .value
                .as_ref()
                .unwrap()
                .value
                .clone()
        };
        assert!(matches!(get("i"), Some(PV::IntValue(42))));
        assert!(
            matches!(get("f"), Some(PV::DoubleValue(d)) if (d - std::f64::consts::PI).abs() < 1e-9)
        );
        assert!(matches!(get("b"), Some(PV::BoolValue(true))));
        assert!(matches!(get("by"), Some(PV::BytesValue(ref v)) if v == &vec![1u8,2,3]));
    }

    #[test]
    fn list_attribute_maps_to_array_value() {
        let mut span = minimal_span();
        span.attributes.insert(
            bs("list"),
            AttributeValue::List(vec![AttributeValue::Int(1), AttributeValue::Int(2)]),
        );
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s.attributes.iter().find(|a| a.key == "list").unwrap();
        match kv.value.as_ref().unwrap().value {
            Some(PV::ArrayValue(ref arr)) => {
                assert_eq!(arr.values.len(), 2);
                assert!(matches!(arr.values[0].value, Some(PV::IntValue(1))));
                assert!(matches!(arr.values[1].value, Some(PV::IntValue(2))));
            }
            ref other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn keyvalue_attribute_maps_to_kvlist_value() {
        let mut span = minimal_span();
        let mut nested = crate::span::vec_map::VecMap::new();
        nested.insert(bs("nested_key"), AttributeValue::String(bs("nested_val")));
        span.attributes
            .insert(bs("kv"), AttributeValue::KeyValue(nested));
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let kv = s.attributes.iter().find(|a| a.key == "kv").unwrap();
        match kv.value.as_ref().unwrap().value {
            Some(PV::KvlistValue(ref list)) => {
                assert_eq!(list.values.len(), 1);
                assert_eq!(list.values[0].key, "nested_key");
                match list.values[0].value.as_ref().unwrap().value {
                    Some(PV::StringValue(ref v)) => assert_eq!(v, "nested_val"),
                    ref other => panic!("expected string, got {other:?}"),
                }
            }
            ref other => panic!("expected kvlist, got {other:?}"),
        }
    }

    #[test]
    fn span_link_masks_sentinel_bit_and_uses_direct_trace_id() {
        let mut span = minimal_span();
        span.span_links.push(SpanLink {
            trace_id: [0xCD; 16],
            span_id: 0x22,
            flags: 0x8000_0001,
            ..Default::default()
        });
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let link = &req.resource_spans[0].scope_spans[0].spans[0].links[0];
        assert_eq!(link.trace_id, vec![0xCD; 16]);
        assert_eq!(link.flags, 1, "sentinel bit must be masked out");
    }

    #[test]
    fn span_link_attributes_are_typed_not_stringified() {
        let mut span = minimal_span();
        let mut link = SpanLink {
            trace_id: [0xCD; 16],
            span_id: 0x22,
            ..Default::default()
        };
        link.attributes.insert(bs("count"), AttributeValue::Int(7));
        span.span_links.push(link);
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let link = &req.resource_spans[0].scope_spans[0].spans[0].links[0];
        let kv = link.attributes.iter().find(|a| a.key == "count").unwrap();
        assert!(matches!(
            kv.value.as_ref().unwrap().value,
            Some(PV::IntValue(7))
        ));
    }

    #[test]
    fn span_event_maps_typed_attributes() {
        let mut span = minimal_span();
        let mut ev = SpanEvent {
            time_unix_nano: 123,
            name: bs("evt"),
            ..Default::default()
        };
        ev.attributes.insert(bs("k"), AttributeValue::Bool(false));
        span.span_events.push(ev);
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let e = &req.resource_spans[0].scope_spans[0].spans[0].events[0];
        assert_eq!(e.time_unix_nano, 123);
        assert_eq!(e.name, "evt");
        let kv = e.attributes.iter().find(|a| a.key == "k").unwrap();
        assert!(matches!(
            kv.value.as_ref().unwrap().value,
            Some(PV::BoolValue(false))
        ));
    }

    #[test]
    fn otel_trace_semantics_enabled_omits_promoted_attrs_and_error_compat_tags() {
        let mut span = minimal_span();
        span.service = bs("span-svc");
        span.r#type = bs("web");
        span.error = true;
        span.attributes
            .insert(bs("error.message"), AttributeValue::String(bs("boom")));
        span.attributes
            .insert(bs("http.method"), AttributeValue::String(bs("GET")));
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(
            &[chunk],
            &OtlpResourceInfo {
                service: "resource-svc".to_string(),
                ..Default::default()
            },
            true,
        );
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        let keys: Vec<&str> = s.attributes.iter().map(|kv| kv.key.as_str()).collect();
        for omitted in [
            "service.name",
            "operation.name",
            "resource.name",
            "span.type",
            "error.message",
        ] {
            assert!(!keys.contains(&omitted), "must omit {omitted}");
        }
        assert!(keys.contains(&"http.method"));
        let status = s.status.as_ref().unwrap();
        assert_eq!(
            status.message, "boom",
            "error message still promoted to Status"
        );
    }

    #[test]
    fn empty_chunk_does_not_panic() {
        let chunk = TraceChunk::<BytesData> {
            trace_id: [0; 16],
            spans: vec![],
            ..Default::default()
        };
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        assert!(req.resource_spans[0].scope_spans[0].spans.is_empty());
    }

    #[test]
    fn resource_name_maps_to_span_name_and_resource_name_attribute() {
        let mut span = minimal_span();
        span.resource = bs("GET /api/users");
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.name, "GET /api/users");
        let kv = s
            .attributes
            .iter()
            .find(|a| a.key == "resource.name")
            .unwrap();
        match kv.value.as_ref().unwrap().value {
            Some(PV::StringValue(ref v)) => assert_eq!(v, "GET /api/users"),
            ref other => panic!("expected string, got {other:?}"),
        }
    }

    #[test]
    fn tracestate_attribute_maps_to_trace_state_field() {
        let mut span = minimal_span();
        span.attributes
            .insert(bs("tracestate"), AttributeValue::String(bs("vendor1=abc")));
        let chunk = minimal_chunk([1; 16], span);
        let req = map_traces_to_otlp_v1(&[chunk], &OtlpResourceInfo::default(), false);
        let s = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(s.trace_state, "vendor1=abc");
    }
}
