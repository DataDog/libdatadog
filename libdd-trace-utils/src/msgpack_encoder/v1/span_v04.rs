// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
use crate::span::TraceData;
use rmp::encode::{
    write_bin, write_bool, write_f64, write_i64, write_sint, write_u64, write_uint, write_uint8,
    RmpWrite, ValueWriteError,
};
use std::borrow::Borrow;

use super::StringTable;

/// Integer keys for V1 span fields.
#[repr(u8)]
pub(super) enum SpanKey {
    Service = 1,
    Name = 2,
    Resource = 3,
    SpanId = 4,
    ParentId = 5,
    Start = 6,
    Duration = 7,
    Error = 8,
    Attributes = 9,
    Type = 10,
    SpanLinks = 11,
    SpanEvents = 12,
    Env = 13,
    Version = 14,
    Component = 15,
    Kind = 16,
}

/// Integer keys for V1 span link fields.
#[repr(u8)]
pub(super) enum SpanLinkKey {
    TraceId = 1,
    SpanId = 2,
    Attributes = 3,
    TraceState = 4,
    Flags = 5,
}

/// Integer keys for V1 span event fields.
#[repr(u8)]
pub(super) enum SpanEventKey {
    Time = 1,
    Name = 2,
    Attributes = 3,
}

/// Type discriminants for attribute values.
/// An attribute value is encoded as [type_uint8][actual_value].
#[repr(u8)]
pub(super) enum AnyValueKey {
    String = 1,
    Bool = 2,
    Double = 3,
    Bytes = 5,
}

/// Maps the `span.kind` string tag (from v0.4 meta) to the OTEL SpanKind uint32.
fn span_kind_from_str(s: &str) -> Option<u32> {
    match s {
        "internal" => Some(1),
        "server" => Some(2),
        "client" => Some(3),
        "producer" => Some(4),
        "consumer" => Some(5),
        _ => None,
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

        if !link.tracestate.borrow().is_empty() {
            write_uint8(writer, SpanLinkKey::TraceState as u8)?;
            table.write_interned(writer, link.tracestate.borrow())?;
        }

        if link.flags != 0 {
            write_uint8(writer, SpanLinkKey::Flags as u8)?;
            write_uint(writer, link.flags as u64)?;
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
            AttributeArrayValue::Integer(i) => {
                write_uint8(writer, 4u8)?; // Int64
                write_sint(writer, *i)?;
            }
            AttributeArrayValue::Double(d) => {
                write_uint8(writer, AnyValueKey::Double as u8)?;
                write_f64(writer, *d)?;
            }
        }
        Ok(())
    }

    match attribute {
        AttributeAnyValue::SingleValue(value) => {
            encode_array_element(writer, value, table)?;
        }
        AttributeAnyValue::Array(array) => {
            write_uint8(writer, 6u8)?; // Array
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
    // Extract promoted fields from meta — these get dedicated span-level keys and must
    // not appear in the attributes array.
    let env = span.meta.get("env").map(|v| v.borrow());
    let version = span.meta.get("version").map(|v| v.borrow());
    let component = span.meta.get("component").map(|v| v.borrow());
    let kind = span
        .meta
        .get("span.kind")
        .and_then(|v| span_kind_from_str(v.borrow()));

    let is_promoted =
        |k: &T::Text| matches!(k.borrow(), "env" | "version" | "component" | "span.kind");

    let non_promoted_meta = span.meta.iter().filter(|(k, _)| !is_promoted(k)).count() as u32;
    let attr_count = non_promoted_meta + span.metrics.len() as u32 + span.meta_struct.len() as u32;
    let has_attributes = attr_count > 0;

    let span_len = 2 // span_id, start — always present
        + (!span.service.borrow().is_empty()) as u32
        + (!span.name.borrow().is_empty()) as u32
        + (!span.resource.borrow().is_empty()) as u32
        + (!span.r#type.borrow().is_empty()) as u32
        + (span.parent_id != 0) as u32
        + (span.duration != 0) as u32
        + (span.error != 0) as u32
        + has_attributes as u32
        + (!span.span_links.is_empty()) as u32
        + (!span.span_events.is_empty()) as u32
        + env.is_some() as u32
        + version.is_some() as u32
        + component.is_some() as u32
        + kind.is_some() as u32;

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
    write_i64(writer, span.start)?;

    if span.parent_id != 0 {
        write_uint8(writer, SpanKey::ParentId as u8)?;
        write_u64(writer, span.parent_id)?;
    }

    if span.duration != 0 {
        write_uint8(writer, SpanKey::Duration as u8)?;
        write_i64(writer, span.duration)?;
    }

    if span.error != 0 {
        write_uint8(writer, SpanKey::Error as u8)?;
        write_bool(writer, span.error != 0).map_err(ValueWriteError::InvalidDataWrite)?;
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

        for (k, v) in span.meta.iter() {
            if is_promoted(k) {
                continue;
            }
            table.write_interned(writer, k.borrow())?;
            write_uint8(writer, AnyValueKey::String as u8)?;
            table.write_interned(writer, v.borrow())?;
        }

        for (k, v) in span.metrics.iter() {
            table.write_interned(writer, k.borrow())?;
            write_uint8(writer, AnyValueKey::Double as u8)?;
            write_f64(writer, *v)?;
        }

        for (k, v) in span.meta_struct.iter() {
            table.write_interned(writer, k.borrow())?;
            write_uint8(writer, AnyValueKey::Bytes as u8)?;
            write_bin(writer, v.borrow())?;
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
    if let Some(k) = kind {
        write_uint8(writer, SpanKey::Kind as u8)?;
        write_uint(writer, k as u64)?;
    }

    Ok(())
}
