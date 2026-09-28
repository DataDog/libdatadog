// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native V1 span encoder: `crate::span::v1::Span` → V1 msgpack wire.
//! (Convention documented in [`crate::msgpack_encoder`].)

use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanLink};
use crate::span::vec_map::VecMap;
use crate::span::TraceData;
use rmp::encode::{
    write_array_len, write_bin, write_bool, write_f64, write_map_len, write_sint, write_u64,
    write_uint, write_uint8, RmpWrite, ValueWriteError,
};
use std::borrow::Borrow;

use super::{
    normalize_span_start, AnyValueKey, SpanEventKey, SpanKey, SpanLinkKey, StringTable,
    FLAT_ATTR_STRIDE, TYPED_VALUE_STRIDE,
};

/// Encodes a typed `AttributeValue` as `[type_uint8, value]`.
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `value` - The attribute value to encode.
/// * `table` - The streaming string intern table used for interning string values.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_attribute_value<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    value: &AttributeValue<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    match value {
        AttributeValue::String(s) => {
            write_uint8(writer, AnyValueKey::String as u8)?;
            table.write_interned(writer, s.borrow())?;
        }
        AttributeValue::Bool(b) => {
            write_uint8(writer, AnyValueKey::Bool as u8)?;
            write_bool(writer, *b).map_err(ValueWriteError::InvalidDataWrite)?;
        }
        AttributeValue::Float(f) => {
            write_uint8(writer, AnyValueKey::Double as u8)?;
            write_f64(writer, *f)?;
        }
        AttributeValue::Int(i) => {
            write_uint8(writer, AnyValueKey::Int64 as u8)?;
            write_sint(writer, *i)?;
        }
        AttributeValue::Bytes(b) => {
            write_uint8(writer, AnyValueKey::Bytes as u8)?;
            write_bin(writer, b.borrow())?;
        }
        AttributeValue::List(arr) => {
            // Encoded as a flat array of `[type, value]` pairs.
            write_uint8(writer, AnyValueKey::Array as u8)?;
            write_array_len(writer, arr.len() as u32 * TYPED_VALUE_STRIDE)?;
            for v in arr {
                encode_attribute_value(writer, v, table)?;
            }
        }
        AttributeValue::KeyValue(map) => {
            // Encoded as a flat array of `[key, type, value]` triplets — consistent with the
            // top-level attributes map (`encode_attributes_map`).
            write_uint8(writer, AnyValueKey::KeyValueList as u8)?;
            encode_attributes_map(writer, map, table)?;
        }
    }
    Ok(())
}

/// Encodes a map of attributes as a flat triplet array: `[key, type_uint8, value, ...]`.
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `map` - The attribute map to encode.
/// * `table` - The streaming string intern table used for interning keys and string values.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_attributes_map<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    map: &VecMap<T::Text, AttributeValue<T>>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    // `VecMap` tolerates duplicate keys for fast insertion; `defensive_dedup` returns a view
    // with each key emitted once (last-write-wins) and warns if the map wasn't already deduped
    // before encoding.
    let deduped = map.defensive_dedup();
    write_array_len(writer, deduped.len() as u32 * FLAT_ATTR_STRIDE)?;
    for (k, v) in deduped.iter() {
        table.write_interned(writer, k.borrow())?;
        encode_attribute_value(writer, v, table)?;
    }
    Ok(())
}

/// Encodes a [`v1::SpanLink`](crate::span::v1::SpanLink) into the V1 msgpack wire format
/// (native encoding: V1 input → V1 output).
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `span_links` - The span links to encode.
/// * `table` - The streaming string intern table.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_span_links<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_links: &[SpanLink<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    write_uint8(writer, SpanKey::SpanLinks as u8)?;
    write_array_len(writer, span_links.len() as u32)?;

    for link in span_links {
        let link_len = 1 // trace_id (always)
            + (link.span_id != 0) as u32
            + (!link.attributes.is_empty()) as u32
            + (!link.tracestate.borrow().is_empty()) as u32
            + (link.flags != 0) as u32;

        write_map_len(writer, link_len)?;

        write_uint8(writer, SpanLinkKey::TraceId as u8)?;
        write_bin(writer, &link.trace_id)?;

        if link.span_id != 0 {
            write_uint8(writer, SpanLinkKey::SpanId as u8)?;
            write_u64(writer, link.span_id)?;
        }

        if !link.attributes.is_empty() {
            write_uint8(writer, SpanLinkKey::Attributes as u8)?;
            encode_attributes_map(writer, &link.attributes, table)?;
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

/// Encodes a [`v1::SpanEvent`](crate::span::v1::SpanEvent) into the V1 msgpack wire format
/// (native encoding: V1 input → V1 output).
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `span_events` - The span events to encode.
/// * `table` - The streaming string intern table.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_span_events<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span_events: &[SpanEvent<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    write_uint8(writer, SpanKey::SpanEvents as u8)?;
    write_array_len(writer, span_events.len() as u32)?;

    for event in span_events {
        let event_len = 2 // time + name
            + (!event.attributes.is_empty()) as u32;

        write_map_len(writer, event_len)?;

        write_uint8(writer, SpanEventKey::Time as u8)?;
        write_u64(writer, event.time_unix_nano)?;

        write_uint8(writer, SpanEventKey::Name as u8)?;
        table.write_interned(writer, event.name.borrow())?;

        if !event.attributes.is_empty() {
            write_uint8(writer, SpanEventKey::Attributes as u8)?;
            encode_attributes_map(writer, &event.attributes, table)?;
        }
    }

    Ok(())
}

/// Encodes a [`v1::Span`](crate::span::v1::Span) into the V1 msgpack wire format
/// (native encoding: V1 input → V1 output).
///
/// # Arguments
///
/// * `writer` - A RmpWriter compatible with rmp writing functions.
/// * `span` - The span to encode.
/// * `table` - The streaming string intern table.
///
/// # Returns
///
/// * `Ok(())` - Nothing if successful.
/// * `Err(ValueWriteError)` - An error if the writing fails.
///
/// # Errors
///
/// This function will return any error emitted by the writer.
pub(super) fn encode_span<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    span: &Span<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    let is_parent = span.parent_id != 0;
    let has_duration = span.duration != 0;
    let has_error = span.error;
    let has_attributes = !span.attributes.is_empty();
    let has_env = !span.env.borrow().is_empty();
    let has_version = !span.version.borrow().is_empty();
    let has_component = !span.component.borrow().is_empty();

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
        + has_env as u32
        + has_version as u32
        + has_component as u32;

    write_map_len(writer, span_len)?;

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
        write_u64(writer, span.duration.max(0) as u64)?;
    }

    if has_error {
        write_uint8(writer, SpanKey::Error as u8)?;
        write_bool(writer, true).map_err(ValueWriteError::InvalidDataWrite)?;
    }

    if !span.r#type.borrow().is_empty() {
        write_uint8(writer, SpanKey::Type as u8)?;
        table.write_interned(writer, span.r#type.borrow())?;
    }

    if has_attributes {
        write_uint8(writer, SpanKey::Attributes as u8)?;
        encode_attributes_map(writer, &span.attributes, table)?;
    }

    if !span.span_links.is_empty() {
        encode_span_links(writer, &span.span_links, table)?;
    }

    if !span.span_events.is_empty() {
        encode_span_events(writer, &span.span_events, table)?;
    }

    if has_env {
        write_uint8(writer, SpanKey::Env as u8)?;
        table.write_interned(writer, span.env.borrow())?;
    }
    if has_version {
        write_uint8(writer, SpanKey::Version as u8)?;
        table.write_interned(writer, span.version.borrow())?;
    }
    if has_component {
        write_uint8(writer, SpanKey::Component as u8)?;
        table.write_interned(writer, span.component.borrow())?;
    }
    // SpanKind is always emitted (default = Internal).
    write_uint8(writer, SpanKey::Kind as u8)?;
    write_uint(writer, span.span_kind as u64)?;

    Ok(())
}
