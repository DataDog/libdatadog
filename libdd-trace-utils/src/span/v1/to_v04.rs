// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Conversion from a v1 [`TracePayload`] to the v04 span representation.
//!
//! The v04 format is flat: each v1 chunk maps to one `Vec<v04::Span>`.
//! Fields that exist in v1 but have no direct v04 counterpart are placed as
//! entries in `meta` / `metrics` / `meta_struct` on the first span in the
//! relevant scope:
//!
//! * Chunk-level metadata → first span of each chunk.
//! * Trace-level metadata → first span of the **first** chunk.
//!
//! Span-level fields take priority over chunk-level, which in turn take
//! priority over trace-level (higher-specificity values are never overwritten).
//!
//! ## Attribute value mapping
//!
//! | v1 type  | v04 destination                                               |
//! |----------|---------------------------------------------------------------|
//! | String   | `meta`                                                        |
//! | Bytes    | `meta_struct`                                                 |
//! | Boolean  | `metrics` (0.0 / 1.0)                                        |
//! | Integer  | `metrics` if \|value\| ≤ 2^53, else `meta` as decimal string |
//! | Double   | `metrics`                                                     |
//! | Array    | expanded with dot-notation index keys (`k.0`, `k.1`, …)      |
//! | Map      | expanded with dot-notation string keys (`k.child`, …)        |

use std::borrow::Borrow;
use std::collections::HashMap;
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{BytesData, span_kind_to_str};
use crate::span::table::TraceStringRef;
use crate::span::v1::{
    AttributeAnyValue as V1Value, Span as V1Span, SpanEvent as V1SpanEvent,
    SpanLink as V1SpanLink, TracePayload, TraceStaticData,
};
use crate::span::v04::{
    AttributeAnyValue as V04AnyValue, AttributeArrayValue as V04ArrayValue,
    Span as V04Span, SpanEvent as V04SpanEvent, SpanLink as V04SpanLink,
};

fn resolve(data: &TraceStaticData<BytesData>, r: TraceStringRef) -> &str {
    let s: &BytesString = data.get_string(r);
    s.borrow()
}

fn dot_join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() { key.to_owned() } else { format!("{prefix}.{key}") }
}

/// Convert a v1 [`TracePayload`] to the v04 representation.
///
/// Returns one `Vec<v04::Span<BytesData>>` per v1 chunk.
pub fn to_v04(payload: &TracePayload<BytesData>) -> Vec<Vec<V04Span<BytesData>>> {
    let data = &payload.static_data;
    let traces = &payload.traces;

    traces.chunks.iter().map(|chunk| {
        // Convert each span individually.
        let mut spans: Vec<V04Span<BytesData>> = chunk.spans.iter()
            .map(|s| convert_span(s, data, chunk.trace_id))
            .collect();

        if let Some(first) = spans.first_mut() {
            // Chunk&Trace-level fields → first span (do not overwrite span-level values).
            let origin = resolve(data, chunk.origin);
            if !origin.is_empty() {
                first.meta.entry(BytesString::from("_dd.origin")).or_insert_with(|| BytesString::from(origin));
            }
            if chunk.priority != 0 {
                first.metrics.entry(BytesString::from("_sampling_priority_v1"))
                    .or_insert(chunk.priority as f64);
            }
            if chunk.sampling_mechanism != 0 {
                first.metrics.entry(BytesString::from("_dd.mechanism"))
                    .or_insert(chunk.sampling_mechanism as f64);
            }
            if chunk.dropped_trace {
                first.meta.entry(BytesString::from("_dd.dropped")).or_insert_with(|| BytesString::from("true"));
            }
            flatten_attrs_no_overwrite(&chunk.attributes, data, "", first);

            let container_id = resolve(data, traces.container_id);
            if !container_id.is_empty() {
                first.meta.entry(BytesString::from("container_id")).or_insert_with(|| BytesString::from(container_id));
            }
            let lang = resolve(data, traces.language_name);
            if !lang.is_empty() {
                first.meta.entry(BytesString::from("language")).or_insert_with(|| BytesString::from(lang));
            }
            let lang_ver = resolve(data, traces.language_version);
            if !lang_ver.is_empty() {
                first.meta.entry(BytesString::from("language_version")).or_insert_with(|| BytesString::from(lang_ver));
            }
            let tracer_ver = resolve(data, traces.tracer_version);
            if !tracer_ver.is_empty() {
                first.meta.entry(BytesString::from("tracer_version")).or_insert_with(|| BytesString::from(tracer_ver));
            }
            let runtime_id = resolve(data, traces.runtime_id);
            if !runtime_id.is_empty() {
                first.meta.entry(BytesString::from("runtime-id")).or_insert_with(|| BytesString::from(runtime_id));
            }
            let env = resolve(data, traces.env);
            if !env.is_empty() {
                first.meta.entry(BytesString::from("env")).or_insert_with(|| BytesString::from(env));
            }
            let hostname = resolve(data, traces.hostname);
            if !hostname.is_empty() {
                first.meta.entry(BytesString::from("_dd.hostname")).or_insert_with(|| BytesString::from(hostname));
            }
            let app_version = resolve(data, traces.app_version);
            if !app_version.is_empty() {
                first.meta.entry(BytesString::from("version")).or_insert_with(|| BytesString::from(app_version));
            }
            flatten_attrs_no_overwrite(&traces.attributes, data, "", first);
        }

        spans
    }).collect()
}

fn convert_span(
    v1_span: &V1Span,
    data: &TraceStaticData<BytesData>,
    trace_id: u128,
) -> V04Span<BytesData> {
    let mut span = V04Span::<BytesData>::default();

    span.service = BytesString::from(resolve(data, v1_span.service));
    span.name = BytesString::from(resolve(data, v1_span.name));
    span.resource = BytesString::from(resolve(data, v1_span.resource));
    span.r#type = BytesString::from(resolve(data, v1_span.r#type));
    span.trace_id = trace_id;
    span.span_id = v1_span.span_id;
    span.parent_id = v1_span.parent_id;
    span.start = v1_span.start;
    span.duration = v1_span.duration;
    span.error = v1_span.error as i32;

    let env = resolve(data, v1_span.env);
    if !env.is_empty() {
        span.meta.insert(BytesString::from("env"), BytesString::from(env));
    }
    let version = resolve(data, v1_span.version);
    if !version.is_empty() {
        span.meta.insert(BytesString::from("version"), BytesString::from(version));
    }
    let component = resolve(data, v1_span.component);
    if !component.is_empty() {
        span.meta.insert(BytesString::from("component"), BytesString::from(component));
    }

    if v1_span.kind != SpanKind::Internal {
        if let Some(kind_str) = span_kind_to_str(v1_span.kind) {
            span.meta.insert(BytesString::from("span.kind"), BytesString::from(kind_str));
        }
    }

    flatten_attrs(&v1_span.attributes, data, "",
        &mut span.meta, &mut span.metrics, &mut span.meta_struct);

    span.span_links = v1_span.span_links.iter().map(|l| convert_link(l, data)).collect();
    span.span_events = v1_span.span_events.iter().map(|e| convert_event(e, data)).collect();

    span
}

fn convert_link(link: &V1SpanLink, data: &TraceStaticData<BytesData>) -> V04SpanLink<BytesData> {
    let mut out = V04SpanLink::<BytesData>::default();
    out.trace_id = link.trace_id as u64;
    out.trace_id_high = (link.trace_id >> 64) as u64;
    out.span_id = link.span_id;
    out.tracestate = BytesString::from(resolve(data, link.tracestate));
    out.flags = link.flags;
    // v04 span-link attributes are string-only.
    for (k, v) in &link.attributes {
        out.attributes.insert(
            BytesString::from(resolve(data, *k)),
            BytesString::from(value_to_string(v, data)),
        );
    }
    out
}

fn convert_event(event: &V1SpanEvent, data: &TraceStaticData<BytesData>) -> V04SpanEvent<BytesData> {
    let mut out = V04SpanEvent::<BytesData>::default();
    out.time_unix_nano = event.time_unix_nano;
    out.name = BytesString::from(resolve(data, event.name));
    for (k, v) in &event.attributes {
        out.attributes.insert(
            BytesString::from(resolve(data, *k)),
            v1_to_v04_any_value(v, data),
        );
    }
    out
}

fn v1_to_v04_any_value(v: &V1Value, data: &TraceStaticData<BytesData>) -> V04AnyValue<BytesData> {
    match v {
        V1Value::String(s) =>
            V04AnyValue::SingleValue(V04ArrayValue::String(BytesString::from(resolve(data, *s)))),
        V1Value::Boolean(b) =>
            V04AnyValue::SingleValue(V04ArrayValue::Boolean(*b)),
        V1Value::Integer(i) =>
            V04AnyValue::SingleValue(V04ArrayValue::Integer(*i)),
        V1Value::Double(d) =>
            V04AnyValue::SingleValue(V04ArrayValue::Double(*d)),
        V1Value::Bytes(b) => {
            let bytes: &[u8] = data.get_bytes(*b).borrow();
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            V04AnyValue::SingleValue(V04ArrayValue::String(BytesString::from(hex)))
        }
        V1Value::Array(arr) =>
            V04AnyValue::Array(arr.iter().map(|elem| v1_to_v04_array_elem(elem, data)).collect()),
        V1Value::Map(_) =>
            V04AnyValue::SingleValue(V04ArrayValue::String(BytesString::from(value_to_string(v, data)))),
    }
}

fn v1_to_v04_array_elem(v: &V1Value, data: &TraceStaticData<BytesData>) -> V04ArrayValue<BytesData> {
    match v {
        V1Value::String(s) => V04ArrayValue::String(BytesString::from(resolve(data, *s))),
        V1Value::Boolean(b) => V04ArrayValue::Boolean(*b),
        V1Value::Integer(i) => V04ArrayValue::Integer(*i),
        V1Value::Double(d) => V04ArrayValue::Double(*d),
        // Bytes, nested arrays, maps → string representation.
        _ => V04ArrayValue::String(BytesString::from(value_to_string(v, data))),
    }
}

/// Flatten v1 attributes into v04 meta/metrics/meta_struct, overwriting
/// any existing keys.
fn flatten_attrs(
    attrs: &HashMap<TraceStringRef, V1Value>,
    data: &TraceStaticData<BytesData>,
    prefix: &str,
    meta: &mut HashMap<BytesString, BytesString>,
    metrics: &mut HashMap<BytesString, f64>,
    meta_struct: &mut HashMap<BytesString, Bytes>,
) {
    for (k, v) in attrs {
        let full_key = dot_join(prefix, resolve(data, *k));
        flatten_value(&full_key, v, data, meta, metrics, meta_struct);
    }
}

/// Like [`flatten_attrs`] but uses `entry().or_insert*` so already-set values
/// (from span-level conversion) are not overwritten.
fn flatten_attrs_no_overwrite(
    attrs: &HashMap<TraceStringRef, V1Value>,
    data: &TraceStaticData<BytesData>,
    prefix: &str,
    span: &mut V04Span<BytesData>,
) {
    for (k, v) in attrs {
        let full_key = dot_join(prefix, resolve(data, *k));
        flatten_value_no_overwrite(&full_key, v, data, span);
    }
}

fn flatten_value(
    key: &str,
    v: &V1Value,
    data: &TraceStaticData<BytesData>,
    meta: &mut HashMap<BytesString, BytesString>,
    metrics: &mut HashMap<BytesString, f64>,
    meta_struct: &mut HashMap<BytesString, Bytes>,
) {
    match v {
        V1Value::String(s) => {
            meta.insert(BytesString::from(key), BytesString::from(resolve(data, *s)));
        }
        V1Value::Bytes(b) => {
            meta_struct.insert(BytesString::from(key), data.get_bytes(*b).clone());
        }
        V1Value::Boolean(b) => {
            metrics.insert(BytesString::from(key), if *b { 1.0 } else { 0.0 });
        }
        V1Value::Integer(i) => {
            if (*i).unsigned_abs() <= (1u64 << 53) {
                metrics.insert(BytesString::from(key), *i as f64);
            } else {
                let s = i.to_string();
                meta.insert(BytesString::from(key), BytesString::from(s));
            }
        }
        V1Value::Double(d) => {
            metrics.insert(BytesString::from(key), *d);
        }
        V1Value::Array(arr) => {
            for (idx, elem) in arr.iter().enumerate() {
                let child_key = dot_join(key, &idx.to_string());
                flatten_value(&child_key, elem, data, meta, metrics, meta_struct);
            }
        }
        V1Value::Map(map) => {
            for (child_k, child_v) in map {
                let child_key = dot_join(key, resolve(data, *child_k));
                flatten_value(&child_key, child_v, data, meta, metrics, meta_struct);
            }
        }
    }
}

fn flatten_value_no_overwrite(
    key: &str,
    v: &V1Value,
    data: &TraceStaticData<BytesData>,
    span: &mut V04Span<BytesData>,
) {
    match v {
        V1Value::String(s) => {
            span.meta.entry(BytesString::from(key)).or_insert_with(|| BytesString::from(resolve(data, *s)));
        }
        V1Value::Bytes(b) => {
            span.meta_struct.entry(BytesString::from(key))
                .or_insert_with(|| data.get_bytes(*b).clone());
        }
        V1Value::Boolean(b) => {
            span.metrics.entry(BytesString::from(key)).or_insert(if *b { 1.0 } else { 0.0 });
        }
        V1Value::Integer(i) => {
            if (*i).unsigned_abs() <= (1u64 << 53) {
                span.metrics.entry(BytesString::from(key)).or_insert(*i as f64);
            } else {
                let s = i.to_string();
                span.meta.entry(BytesString::from(key)).or_insert_with(|| BytesString::from(s));
            }
        }
        V1Value::Double(d) => {
            span.metrics.entry(BytesString::from(key)).or_insert(*d);
        }
        V1Value::Array(arr) => {
            for (idx, elem) in arr.iter().enumerate() {
                let child_key = dot_join(key, &idx.to_string());
                flatten_value_no_overwrite(&child_key, elem, data, span);
            }
        }
        V1Value::Map(map) => {
            for (child_k, child_v) in map {
                let child_key = dot_join(key, resolve(data, *child_k));
                flatten_value_no_overwrite(&child_key, child_v, data, span);
            }
        }
    }
}

/// String representation (for span-link attributes and nested types)
/// Convert a v1 attribute value to a string.  Used in contexts where v04 only
/// supports strings (span-link attributes, or nested values inside arrays).
/// HashMap keys are sorted to produce a deterministic output.
fn value_to_string(v: &V1Value, data: &TraceStaticData<BytesData>) -> String {
    match v {
        V1Value::String(s) => resolve(data, *s).to_owned(),
        V1Value::Boolean(b) => b.to_string(),
        V1Value::Integer(i) => i.to_string(),
        V1Value::Double(d) => d.to_string(),
        V1Value::Bytes(b) => {
            let bytes: &[u8] = data.get_bytes(*b).borrow();
            bytes.iter().map(|byte| format!("{byte:02x}")).collect()
        }
        V1Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(|v| value_to_string(v, data)).collect();
            format!("[{}]", parts.join(","))
        }
        V1Value::Map(map) => {
            let mut parts: Vec<String> = map.iter()
                .map(|(k, v)| {
                    format!("{}:{}", resolve(data, *k), value_to_string(v, data))
                })
                .collect();
            parts.sort(); // deterministic regardless of HashMap iteration order
            format!("{{{}}}", parts.join(","))
        }
    }
}
