// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
use crate::span::TraceData;
use rmp::encode::{
    write_bin, write_bool, write_f64, write_sint, write_u64, write_uint, write_uint8, RmpWrite,
    ValueWriteError,
};
use std::borrow::Borrow;

use super::{normalize_span_start, AnyValueKey, SpanEventKey, SpanKey, SpanLinkKey, StringTable};

/// Maps the `span.kind` string tag (from v0.4 meta) to the OTEL SpanKind uint32.
///
/// Per the OTEL spec, missing or unrecognized values default to `Internal` (1) — this
/// matches the agent's behavior in `pkg/trace/api/converter.go`.
fn span_kind_from_str(s: &str) -> u32 {
    match s {
        "server" => 2,
        "client" => 3,
        "producer" => 4,
        "consumer" => 5,
        // "internal" and any other string fall through to Internal.
        _ => 1,
    }
}

/// Encodes span links into the V1 format.
///
/// Uses integer keys and string interning for string values. Each span link's
/// trace ID is encoded as a 16-byte big-endian binary.
pub fn encode_span_links<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_links: &[SpanLink<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    write_uint8(writer, SpanKey::SpanLinks as u8)?;
    rmp::encode::write_array_len(writer, span_links.len() as u32)?;

    for link in span_links {
        let trace_id_128 = ((link.trace_id_high as u128) << 64) | link.trace_id as u128;
        let link_len = 1 // trace_id (always)
            + (link.span_id != 0) as u32
            + (!link.attributes.is_empty()) as u32
            + (!link.tracestate.borrow().is_empty()) as u32
            + (link.flags != 0) as u32;

        rmp::encode::write_map_len(writer, link_len)?;

        write_uint8(writer, SpanLinkKey::TraceId as u8)?;
        write_bin(writer, &trace_id_128.to_be_bytes())?;

        if link.span_id != 0 {
            write_uint8(writer, SpanLinkKey::SpanId as u8)?;
            write_u64(writer, link.span_id)?;
        }

        if !link.attributes.is_empty() {
            write_uint8(writer, SpanLinkKey::Attributes as u8)?;
            rmp::encode::write_array_len(writer, link.attributes.len() as u32 * 3)?;
            for (k, v) in link.attributes.iter() {
                table.write_interned(writer, k.borrow())?;
                write_uint8(writer, AnyValueKey::String as u8)?;
                table.write_interned(writer, v.borrow())?;
            }
        }

        if !link.tracestate.borrow().is_empty() {
            write_uint8(writer, SpanLinkKey::TraceState as u8)?;
            table.write_interned(writer, link.tracestate.borrow())?;
        }

        if link.flags != 0 {
            write_uint8(writer, SpanLinkKey::Flags as u8)?;
            write_uint(writer, link.flags as u64)?;
        }
    }

    Ok(())
}

/// Encodes span events into the V1 format.
///
/// Uses integer keys and string interning. Attribute values are type-tagged.
pub fn encode_span_events<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_events: &[SpanEvent<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    write_uint8(writer, SpanKey::SpanEvents as u8)?;
    rmp::encode::write_array_len(writer, span_events.len() as u32)?;

    for event in span_events {
        let event_len = 2 // time_unix_nano, name
            + (!event.attributes.is_empty()) as u32;

        rmp::encode::write_map_len(writer, event_len)?;

        write_uint8(writer, SpanEventKey::Time as u8)?;
        write_u64(writer, event.time_unix_nano)?;

        write_uint8(writer, SpanEventKey::Name as u8)?;
        table.write_interned(writer, event.name.borrow())?;

        if !event.attributes.is_empty() {
            write_uint8(writer, SpanEventKey::Attributes as u8)?;
            encode_span_event_attributes(writer, event, table)?;
        }
    }

    Ok(())
}

fn encode_span_event_attributes<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    event: &SpanEvent<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    rmp::encode::write_array_len(writer, event.attributes.len() as u32 * 3)?;
    for (k, attribute) in event.attributes.iter() {
        table.write_interned(writer, k.borrow())?;
        encode_attribute_any_value(writer, attribute, table)?;
    }
    Ok(())
}

fn encode_attribute_any_value<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    attribute: &AttributeAnyValue<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    fn encode_array_element<W: RmpWrite, T: TraceData>(
        writer: &mut W,
        value: &AttributeArrayValue<T>,
        table: &mut StringTable,
    ) -> Result<(), ValueWriteError<W::Error>> {
        match value {
            AttributeArrayValue::String(s) => {
                write_uint8(writer, AnyValueKey::String as u8)?;
                table.write_interned(writer, s.borrow())?;
            }
            AttributeArrayValue::Boolean(b) => {
                write_uint8(writer, AnyValueKey::Bool as u8)?;
                write_bool(writer, *b).map_err(ValueWriteError::InvalidDataWrite)?;
            }
            AttributeArrayValue::Double(d) => {
                write_uint8(writer, AnyValueKey::Double as u8)?;
                write_f64(writer, *d)?;
            }
            AttributeArrayValue::Integer(i) => {
                write_uint8(writer, AnyValueKey::Int64 as u8)?;
                write_sint(writer, *i)?;
            }
        }
        Ok(())
    }

    match attribute {
        AttributeAnyValue::SingleValue(value) => {
            encode_array_element(writer, value, table)?;
        }
        AttributeAnyValue::Array(array) => {
            write_uint8(writer, AnyValueKey::Array as u8)?;
            rmp::encode::write_array_len(writer, array.len() as u32)?;
            for v in array {
                encode_array_element(writer, v, table)?;
            }
        }
    }
    Ok(())
}

/// Encodes a v0.4 span into the V1 msgpack format.
///
/// Key differences from v0.4:
/// - Uses integer keys for all fields.
/// - `meta` and `metrics` are combined into a single `attributes` array (encoded as flat triplets:
///   key, type, value) with type-tagged values. Promoted meta fields are excluded.
/// - `meta_struct` bytes are included in `attributes` as `Bytes` values.
/// - `trace_id` is not encoded in the span (it belongs to the chunk).
/// - `error` is encoded as a boolean.
/// - `env`, `version`, `component`, `span.kind` are promoted from meta to dedicated span fields.
/// - String values use streaming string interning via `StringTable`.
pub fn encode_span<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span: &Span<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    let is_parent = span.parent_id != 0;
    let has_duration = span.duration != 0;
    let has_error = span.error != 0;

    // Extract promoted fields from meta — these get dedicated span-level keys and must
    // not appear in the attributes array. `_dd.p.tid` is consumed to reconstruct the
    // 128-bit chunk-level trace_id and is dropped here so it doesn't appear twice.
    let is_promoted = |k: &T::Text| {
        matches!(
            k.borrow(),
            "env" | "version" | "component" | "span.kind" | "_dd.p.tid"
        )
    };
    let meta_dd = span.meta.defensive_dedup();
    let metrics_dd = span.metrics.defensive_dedup();
    let meta_struct_dd = span.meta_struct.defensive_dedup();

    let non_promoted_meta = meta_dd.iter().filter(|(k, _)| !is_promoted(k)).count() as u32;
    let metrics_len = metrics_dd.len() as u32;
    let meta_struct_len = meta_struct_dd.len() as u32;
    let attr_count = non_promoted_meta + metrics_len + meta_struct_len;
    let has_attributes = attr_count > 0;

    let env = span.meta.get("env").map(|v| v.borrow());
    let version = span.meta.get("version").map(|v| v.borrow());
    let component = span.meta.get("component").map(|v| v.borrow());
    // span.kind is always emitted — defaults to Internal per OTEL spec.
    let kind = span_kind_from_str(span.meta.get("span.kind").map(|v| v.borrow()).unwrap_or(""));

    let span_len = 3 // span_id, start, kind — always present
        + (!span.service.borrow().is_empty()) as u32
        + (!span.name.borrow().is_empty()) as u32
        + (!span.resource.borrow().is_empty()) as u32
        + (!span.r#type.borrow().is_empty()) as u32
        + is_parent as u32
        + has_duration as u32
        + has_error as u32
        + has_attributes as u32
        + (!span.span_links.is_empty()) as u32
        + (!span.span_events.is_empty()) as u32
        + env.is_some() as u32
        + version.is_some() as u32
        + component.is_some() as u32;

    rmp::encode::write_map_len(writer, span_len)?;

    if !span.service.borrow().is_empty() {
        write_uint8(writer, SpanKey::Service as u8)?;
        table.write_interned(writer, span.service.borrow())?;
    }

    if !span.name.borrow().is_empty() {
        write_uint8(writer, SpanKey::Name as u8)?;
        table.write_interned(writer, span.name.borrow())?;
    }

    if !span.resource.borrow().is_empty() {
        write_uint8(writer, SpanKey::Resource as u8)?;
        table.write_interned(writer, span.resource.borrow())?;
    }

    write_uint8(writer, SpanKey::SpanId as u8)?;
    write_u64(writer, span.span_id)?;

    write_uint8(writer, SpanKey::Start as u8)?;
    write_u64(writer, normalize_span_start(span.start))?;

    if is_parent {
        write_uint8(writer, SpanKey::ParentId as u8)?;
        write_u64(writer, span.parent_id)?;
    }

    if has_duration {
        write_uint8(writer, SpanKey::Duration as u8)?;
        if span.duration < 0 {
            write_u64(writer, 0)?;
        } else {
            write_u64(writer, span.duration as u64)?;
        }
    }

    if has_error {
        write_uint8(writer, SpanKey::Error as u8)?;
        write_bool(writer, has_error).map_err(ValueWriteError::InvalidDataWrite)?;
    }

    if !span.r#type.borrow().is_empty() {
        write_uint8(writer, SpanKey::Type as u8)?;
        table.write_interned(writer, span.r#type.borrow())?;
    }

    if has_attributes {
        // Attributes are encoded as a flat array of triplets: [key, type, value, ...].
        // Length is 3× the number of key-value pairs (per V1 spec).
        write_uint8(writer, SpanKey::Attributes as u8)?;
        rmp::encode::write_array_len(writer, attr_count * 3)?;

        for (k, v) in meta_dd.iter() {
            if is_promoted(k) {
                continue;
            }
            table.write_interned(writer, (*k).borrow())?;
            write_uint8(writer, AnyValueKey::String as u8)?;
            table.write_interned(writer, (*v).borrow())?;
        }

        for (k, v) in metrics_dd.iter() {
            table.write_interned(writer, (*k).borrow())?;
            write_uint8(writer, AnyValueKey::Double as u8)?;
            write_f64(writer, *v)?;
        }

        for (k, v) in meta_struct_dd.iter() {
            table.write_interned(writer, (*k).borrow())?;
            write_uint8(writer, AnyValueKey::Bytes as u8)?;
            write_bin(writer, (*v).borrow())?;
        }
    }

    if !span.span_links.is_empty() {
        encode_span_links(writer, &span.span_links, table)?;
    }

    if !span.span_events.is_empty() {
        encode_span_events(writer, &span.span_events, table)?;
    }

    // Promoted span-level fields (env, version, component, span.kind → kind uint32).
    if let Some(v) = env {
        write_uint8(writer, SpanKey::Env as u8)?;
        table.write_interned(writer, v)?;
    }
    if let Some(v) = version {
        write_uint8(writer, SpanKey::Version as u8)?;
        table.write_interned(writer, v)?;
    }
    if let Some(v) = component {
        write_uint8(writer, SpanKey::Component as u8)?;
        table.write_interned(writer, v)?;
    }
    write_uint8(writer, SpanKey::Kind as u8)?;
    write_uint(writer, kind as u64)?;

    Ok(())
}
