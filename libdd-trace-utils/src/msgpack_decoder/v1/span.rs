// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Naming convention: parent module (`v1`) = input wire format being decoded. This file decodes
// a V1 msgpack span into a [`crate::span::v1::Span`].

use super::{
    read_interned_string, skip_unknown_value, span_event_key, span_key, span_link_key, StringTable,
    ANY_VALUE_KEY_ARRAY, ANY_VALUE_KEY_BOOL, ANY_VALUE_KEY_BYTES, ANY_VALUE_KEY_DOUBLE,
    ANY_VALUE_KEY_INT64, ANY_VALUE_KEY_KEY_VALUE_LIST, ANY_VALUE_KEY_STRING, FLAT_ATTR_STRIDE,
    TYPED_VALUE_STRIDE,
};
use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v1::{AttributeValue, Span, SpanEvent, SpanKind, SpanLink, ThinVec};
use crate::span::vec_map::VecMap;
use crate::span::DeserializableTraceData;
use rmp::decode;
use std::borrow::Borrow;

/// Decodes a V1 span (msgpack map with integer keys) into a [`Span<T>`].
///
/// The streaming `StringTable` is shared across the whole payload, so interned references in
/// this span can resolve to strings that appeared in an earlier chunk or payload header.
pub(super) fn decode_span<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<Span<T>, DecodeError>
where
    T::Text: Clone,
{
    let map_len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read V1 span map len".to_owned()))?;

    let mut span = Span::<T>::default();
    let mut has_span_id = false;
    let mut has_start = false;

    for _ in 0..map_len {
        let key = decode::read_int::<u8, _>(buf.as_mut_slice())
            .map_err(|_| DecodeError::InvalidFormat("V1 span key (u8) read failure".to_owned()))?;

        match key {
            span_key::SERVICE => span.service = read_interned_string(buf, table)?,
            span_key::NAME => span.name = read_interned_string(buf, table)?,
            span_key::RESOURCE => span.resource = read_interned_string(buf, table)?,
            span_key::SPAN_ID => {
                span.span_id = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span_id u64 read failure".to_owned())
                })?;
                has_span_id = true;
            }
            span_key::PARENT_ID => {
                span.parent_id = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 parent_id u64 read failure".to_owned())
                })?
            }
            span_key::START => {
                let start: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span start u64 read failure".to_owned())
                })?;
                span.start = i64::try_from(start).map_err(|_| {
                    DecodeError::InvalidFormat(format!("V1 span start {start} exceeds i64::MAX"))
                })?;
                has_start = true;
            }
            span_key::DURATION => {
                let duration: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span duration u64 read failure".to_owned())
                })?;
                span.duration = i64::try_from(duration).map_err(|_| {
                    DecodeError::InvalidFormat(format!(
                        "V1 span duration {duration} exceeds i64::MAX"
                    ))
                })?;
            }
            span_key::ERROR => {
                span.error = decode::read_bool(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span error bool read failure".to_owned())
                })?;
            }
            span_key::TYPE => span.r#type = read_interned_string(buf, table)?,
            span_key::ATTRIBUTES => span.attributes = read_attributes_map(buf, table)?,
            span_key::SPAN_LINKS => span.span_links = read_span_links(buf, table)?,
            span_key::SPAN_EVENTS => span.span_events = read_span_events(buf, table)?,
            span_key::ENV => span.env = read_interned_string(buf, table)?,
            span_key::VERSION => span.version = read_interned_string(buf, table)?,
            span_key::COMPONENT => span.component = read_interned_string(buf, table)?,
            span_key::KIND => {
                let kind: u32 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span_kind u32 read failure".to_owned())
                })?;
                span.span_kind = SpanKind::from(kind);
            }
            _unknown => skip_unknown_value(buf)?,
        }
    }

    if !has_span_id {
        return Err(DecodeError::InvalidFormat(
            "V1 span is missing span_id".to_owned(),
        ));
    }
    if !has_start {
        return Err(DecodeError::InvalidFormat(
            "V1 span is missing start".to_owned(),
        ));
    }

    Ok(span)
}

/// Reads a V1 attributes map encoded as a flat array of `[key, type_uint8, value, ...]` triplets.
pub(super) fn read_attributes_map<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<VecMap<T::Text, AttributeValue<T>>, DecodeError>
where
    T::Text: Clone,
{
    let flat_len = decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("V1 attributes flat array len read failure".to_owned())
    })?;

    if flat_len % FLAT_ATTR_STRIDE != 0 {
        return Err(DecodeError::InvalidFormat(format!(
            "V1 attributes flat array length {flat_len} is not a multiple of {FLAT_ATTR_STRIDE}"
        )));
    }

    let entries = (flat_len / FLAT_ATTR_STRIDE) as usize;
    let mut map = VecMap::with_capacity(buf.capped_capacity(entries));

    for _ in 0..entries {
        let key = read_interned_string(buf, table)?;
        let value = read_typed_attribute_value(buf, table)?;
        map.insert(key, value);
    }

    // V1 attributes are a flat array of triplets, not a real msgpack map, so duplicate keys are
    // possible; leave `deduped` false so re-encoding applies the defensive last-write-wins dedup.
    Ok(map)
}

/// Reads `[type_uint8, value]` and dispatches by type discriminant. Recurses into `Array` and
/// `KeyValueList`.
pub(super) fn read_typed_attribute_value<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<AttributeValue<T>, DecodeError>
where
    T::Text: Clone,
{
    let ty = decode::read_int::<u8, _>(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("V1 attribute type discriminant read failure".to_owned())
    })?;

    match ty {
        ANY_VALUE_KEY_STRING => Ok(AttributeValue::String(read_interned_string(buf, table)?)),
        ANY_VALUE_KEY_BOOL => {
            let b = decode::read_bool(buf.as_mut_slice()).map_err(|_| {
                DecodeError::InvalidFormat("V1 attribute Bool read failure".to_owned())
            })?;
            Ok(AttributeValue::Bool(b))
        }
        ANY_VALUE_KEY_DOUBLE => {
            let f = decode::read_f64(buf.as_mut_slice()).map_err(|_| {
                DecodeError::InvalidFormat("V1 attribute Double read failure".to_owned())
            })?;
            Ok(AttributeValue::Float(f))
        }
        ANY_VALUE_KEY_INT64 => {
            // Encoder writes signed via `write_sint`, which can emit any int marker. `read_int`
            // accepts any integer marker that fits in i64.
            let i: i64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                DecodeError::InvalidFormat("V1 attribute Int64 read failure".to_owned())
            })?;
            Ok(AttributeValue::Int(i))
        }
        ANY_VALUE_KEY_BYTES => Ok(AttributeValue::Bytes(read_bin(buf)?)),
        ANY_VALUE_KEY_ARRAY => {
            let array_len_with_stride =
                decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 attribute Array len read failure".to_owned())
                })?;
            // Array is a flat sequence of [type, value] pairs.
            if array_len_with_stride % TYPED_VALUE_STRIDE != 0 {
                return Err(DecodeError::InvalidFormat(format!(
                    "V1 attribute Array length {array_len_with_stride} is not a multiple of {TYPED_VALUE_STRIDE}"
                )));
            }
            let n = (array_len_with_stride / TYPED_VALUE_STRIDE) as usize;
            let mut items = Vec::with_capacity(buf.capped_capacity(n));
            for _ in 0..n {
                items.push(read_typed_attribute_value(buf, table)?);
            }
            Ok(AttributeValue::List(items))
        }
        ANY_VALUE_KEY_KEY_VALUE_LIST => {
            Ok(AttributeValue::KeyValue(read_attributes_map(buf, table)?))
        }
        unknown => Err(DecodeError::InvalidFormat(format!(
            "Unknown V1 AnyValue type discriminant: {unknown}"
        ))),
    }
}

/// Reads a msgpack `bin` and slices the matching range out of the buffer.
fn read_bin<T: DeserializableTraceData>(buf: &mut Buffer<T>) -> Result<T::Bytes, DecodeError> {
    let len = decode::read_bin_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 bin len read failure".to_owned()))?;
    buf.try_slice_and_advance(len as usize)
        .ok_or_else(|| DecodeError::InvalidFormat("V1 bin payload truncated".to_owned()))
}

/// Reads the span_links array. The `SpanLinks` map key has already been consumed by the caller.
pub(super) fn read_span_links<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<ThinVec<SpanLink<T>>, DecodeError>
where
    T::Text: Clone,
{
    let count = decode::read_array_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 span_links len read failure".to_owned()))?;
    let mut links = ThinVec::with_capacity(buf.capped_capacity(count as usize));
    for _ in 0..count {
        links.push(decode_span_link(buf, table)?);
    }
    Ok(links)
}

fn decode_span_link<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<SpanLink<T>, DecodeError>
where
    T::Text: Clone,
{
    let map_len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 span_link map len read failure".to_owned()))?;
    let mut link = SpanLink::<T>::default();

    for _ in 0..map_len {
        let key = decode::read_int::<u8, _>(buf.as_mut_slice()).map_err(|_| {
            DecodeError::InvalidFormat("V1 span_link key (u8) read failure".to_owned())
        })?;
        match key {
            span_link_key::TRACE_ID => {
                let bytes = read_bin(buf)?;
                let slice: &[u8] = bytes.borrow();
                if slice.len() != 16 {
                    return Err(DecodeError::InvalidFormat(format!(
                        "V1 span_link trace_id expected 16 bytes, got {}",
                        slice.len()
                    )));
                }
                link.trace_id.copy_from_slice(slice);
            }
            span_link_key::SPAN_ID => {
                link.span_id = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span_link span_id read failure".to_owned())
                })?;
            }
            span_link_key::ATTRIBUTES => {
                link.attributes = read_attributes_map(buf, table)?;
            }
            span_link_key::TRACE_STATE => {
                link.tracestate = read_interned_string(buf, table)?;
            }
            span_link_key::FLAGS => {
                let v: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span_link flags read failure".to_owned())
                })?;
                link.flags = u32::try_from(v).map_err(|_| {
                    DecodeError::InvalidFormat(format!("V1 span_link flags {v} exceeds u32::MAX"))
                })?;
            }
            _unknown => skip_unknown_value(buf)?,
        }
    }
    Ok(link)
}

pub(super) fn read_span_events<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<ThinVec<SpanEvent<T>>, DecodeError>
where
    T::Text: Clone,
{
    let count = decode::read_array_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 span_events len read failure".to_owned()))?;
    let mut events = ThinVec::with_capacity(buf.capped_capacity(count as usize));
    for _ in 0..count {
        events.push(decode_span_event(buf, table)?);
    }
    Ok(events)
}

fn decode_span_event<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<SpanEvent<T>, DecodeError>
where
    T::Text: Clone,
{
    let map_len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 span_event map len read failure".to_owned()))?;
    let mut event = SpanEvent::<T>::default();

    for _ in 0..map_len {
        let key = decode::read_int::<u8, _>(buf.as_mut_slice()).map_err(|_| {
            DecodeError::InvalidFormat("V1 span_event key (u8) read failure".to_owned())
        })?;
        match key {
            span_event_key::TIME => {
                event.time_unix_nano = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 span_event time read failure".to_owned())
                })?;
            }
            span_event_key::NAME => {
                event.name = read_interned_string(buf, table)?;
            }
            span_event_key::ATTRIBUTES => {
                event.attributes = read_attributes_map(buf, table)?;
            }
            _unknown => skip_unknown_value(buf)?,
        }
    }
    Ok(event)
}
