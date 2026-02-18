// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::{read_nullable_number, read_num, read_number};
use crate::span::{v1::Span, DeserializableTraceData};
use std::collections::HashMap;
use rmp::{decode, Marker};
use rmp::decode::{read_marker, RmpRead, ValueReadError};
use strum::FromRepr;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::table::{TraceBytesRef, TraceDataRef, TraceStringRef};
use crate::span::v1::{AttributeAnyValue, SpanEvent, SpanLink, TraceChunk, TraceStaticData, Traces};

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum ChunkKey {
    Priority = 1,
    Origin = 2,
    Attributes = 3,
    Spans = 4,
    DroppedTrace = 5,
    TraceId = 6,
    SamplingMechanism = 7,
}

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum TraceKey {
    ContainerId = 2,
    LanguageName = 3,
    LanguageVersion = 4,
    TracerVersion = 5,
    RuntimeId = 6,
    Env = 7,
    Hostname = 8,
    AppVersion = 9,
    Attributes = 10,
    Chunks = 11,
}

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum SpanKey {
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

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum SpanLinkKey {
    TraceId = 1,
    SpanId = 2,
    Attributes = 3,
    TraceState = 4,
    Flags = 5,
}

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum SpanEventKey {
    Time = 1,
    Name = 2,
    Attributes = 3,
}

#[derive(Debug, PartialEq, FromRepr)]
#[repr(u8)]
pub enum AnyValueKey {
    String = 1,
    Bool = 2,
    Double = 3,
    Int64 = 4,
    Bytes = 5,
    Array = 6,
    Map = 7,
}

fn read_string_ref<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<TraceStringRef, DecodeError> {
    match buf.read_string() { // read_string doesn't consume the marker on failure
        Ok(str) => Ok(table.add_string(str)),
        Err(e) => match e {
            DecodeError::InvalidType(_) => {
                Ok(TraceDataRef::new(read_num(buf.as_mut_slice(), false).map_err(|_|
                    DecodeError::InvalidFormat("Bad data type for string".to_owned())
                )?.try_into()?))
            },
            e @ _ => Err(e),
        }
    }
}

fn read_byte_ref<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<TraceBytesRef, DecodeError> {
    let original_slice = buf.as_mut_slice();
    let mut data_slice = *original_slice;
    let byte_array_len = match decode::read_bin_len(&mut data_slice) {
        Ok(len) => len,
        Err(e) => return match e {
            ValueReadError::TypeMismatch(_) => {
                Ok(TraceDataRef::new(read_num(original_slice, false).map_err(|_|
                    DecodeError::InvalidFormat("Bad data type for binary".to_owned())
                )?.try_into()?))
            },
            _ => Err(DecodeError::InvalidFormat("Unable to read binary len for meta_struct".to_owned())),
        }
    };
    *original_slice = data_slice;
    if let Some(data) = buf.try_slice_and_advance(byte_array_len as usize) {
        table.add_bytes(data);
    } else {
        return Err(DecodeError::InvalidFormat(
            "Invalid data length".to_string(),
        ));
    }

    Ok(TraceDataRef::new(decode::read_int(buf.as_mut_slice()).map_err(|_| DecodeError::InvalidFormat("Unable to read ref".to_owned()))?))
}

fn read_array<T: DeserializableTraceData, V, F>(buf: &mut Buffer<T>, mut f: F, vec: &mut Vec<V>) -> Result<(), DecodeError> where F: FnMut(&mut Buffer<T>) -> Result<V, DecodeError> {
    let array_len = decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get array len".to_owned())
    })?;
    for _ in 0..array_len {
        vec.push(f(buf)?)
    }
    Ok(())
}

fn read_key<T: DeserializableTraceData>(buf: &mut Buffer<T>) -> Result<u8, DecodeError> {
    buf.as_mut_slice().read_u8().map_err(|_| DecodeError::InvalidFormat("Could not read protobuf key".to_string()))
}

fn read_trace_id(buf: &mut &'static [u8]) -> Result<u128, DecodeError> {
    let byte_array_len = match read_marker(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read marker for trace_id".to_owned())
    })? {
        Marker::Bin8 => Ok(u32::from(buf.read_data_u8().map_err(|_| {
            DecodeError::InvalidFormat("Unable to byte array size for trace_id".to_owned())
        })?)),
        Marker::Null => return Ok(0),
        _ => Err(DecodeError::InvalidFormat("trace_id is not Bin8 or Null".to_owned()))
    }?;

    if byte_array_len != 16 {
        return Err(DecodeError::InvalidFormat("trace_id must be exactly 16 bytes.".to_owned()))
    }

    if buf.len() < 16 {
        Err(DecodeError::InvalidFormat(
            "Invalid data length".to_string(),
        ))
    } else {
        let trace_id_buf;
        (trace_id_buf, *buf) = buf.split_at(16);
        Ok(u128::from_be_bytes(trace_id_buf.try_into().unwrap()))
    }
}

/// Decodes a slice of bytes into a `Traces` object.
///
/// # Arguments
///
/// * `buf` - A mutable reference to a slice of bytes containing the encoded data.
/// * `table` - A mutable reference to the static data table.
/// * `traces` - A mutable reference to the `Traces` object to be filled.
///
/// # Returns
///
/// * `Ok(())` - If successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The map length cannot be read.
/// - Any key or value cannot be decoded.
pub fn decode_traces<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>, traces: &mut Traces) -> Result<(), DecodeError> {
    let trace_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..trace_size {
        let key = TraceKey::from_repr(read_key(buf)?);
        if let Some(key) = key {
            match key {
                TraceKey::ContainerId => traces.container_id = read_string_ref(buf, table)?,
                TraceKey::LanguageName => traces.language_name = read_string_ref(buf, table)?,
                TraceKey::LanguageVersion => traces.language_version = read_string_ref(buf, table)?,
                TraceKey::TracerVersion => traces.tracer_version = read_string_ref(buf, table)?,
                TraceKey::RuntimeId => traces.runtime_id = read_string_ref(buf, table)?,
                TraceKey::Env => traces.env = read_string_ref(buf, table)?,
                TraceKey::Hostname => traces.hostname = read_string_ref(buf, table)?,
                TraceKey::AppVersion => traces.app_version = read_string_ref(buf, table)?,
                TraceKey::Attributes => decode_attributes(buf, table, &mut traces.attributes)?,
                TraceKey::Chunks => {
                    read_array(buf, |buf| decode_chunk(buf, table), &mut traces.chunks)?;
                }
            }
        } else {
            return Err(DecodeError::InvalidFormat("Invalid traces key".to_owned()))
        }
    }

    Ok(())
}

fn decode_chunk<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<TraceChunk, DecodeError> {
    let mut chunk = TraceChunk::default();

    let chunk_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..chunk_size {
        let key = ChunkKey::from_repr(read_key(buf)?);
        if let Some(key) = key {
            match key {
                ChunkKey::Priority => chunk.priority = read_number(buf)?,
                ChunkKey::Origin => chunk.origin = read_string_ref(buf, table)?,
                ChunkKey::Attributes => decode_attributes(buf, table, &mut chunk.attributes)?,
                ChunkKey::Spans => read_array(buf, |b| decode_span(b, table), &mut chunk.spans)?,
                ChunkKey::DroppedTrace => chunk.dropped_trace = decode::read_bool(buf.as_mut_slice()).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?,
                ChunkKey::TraceId => chunk.trace_id = read_trace_id(buf.as_mut_slice())?,
                ChunkKey::SamplingMechanism => chunk.sampling_mechanism = read_number(buf)?,
            }
        } else {
            return Err(DecodeError::InvalidFormat("Invalid chunk key".to_owned()))
        }
    }

    Ok(chunk)
}

fn decode_span<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<Span, DecodeError> {
    let mut span = Span::default();

    let span_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        let key = SpanKey::from_repr(read_key(buf)?);
        if let Some(key) = key {
            match key {
                SpanKey::Service => span.service = read_string_ref(buf, table)?,
                SpanKey::Name => span.name = read_string_ref(buf, table)?,
                SpanKey::Resource => span.resource = read_string_ref(buf, table)?,
                SpanKey::SpanId => span.span_id = read_number(buf)?,
                SpanKey::ParentId => span.parent_id = read_number(buf)?,
                SpanKey::Start => span.start = read_number(buf)?,
                SpanKey::Duration => span.duration = read_number(buf)?,
                SpanKey::Error => span.error = decode::read_bool(buf.as_mut_slice()).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?,
                SpanKey::Type => span.r#type = read_string_ref(buf, table)?,
                SpanKey::Attributes => decode_attributes(buf, table, &mut span.attributes)?,
                SpanKey::SpanLinks => read_array(buf, |buf| decode_span_link(buf, table), &mut span.span_links)?,
                SpanKey::SpanEvents => read_array(buf, |buf| decode_span_event(buf, table), &mut span.span_events)?,
                SpanKey::Env => span.env = read_string_ref(buf, table)?,
                SpanKey::Version => span.version = read_string_ref(buf, table)?,
                SpanKey::Component => span.component = read_string_ref(buf, table)?,
                SpanKey::Kind => {
                    let kind: i32 = read_nullable_number(buf).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?;
                    span.kind = SpanKind::try_from(kind).map_err(|_| DecodeError::InvalidFormat("Invalid span kind".to_string()))?
                },
            }
        } else {
            return Err(DecodeError::InvalidFormat("Invalid span key".to_owned()))
        }
    }

    Ok(span)
}

fn decode_span_link<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<SpanLink, DecodeError> {
    let mut span_link = SpanLink::default();

    let span_link_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_link_size {
        let key = SpanLinkKey::from_repr(read_key(buf)?);
        if let Some(key) = key {
            match key {
                SpanLinkKey::TraceId => span_link.trace_id = read_trace_id(buf.as_mut_slice())?,
                SpanLinkKey::SpanId => span_link.span_id = read_number(buf)?,
                SpanLinkKey::Attributes => decode_attributes(buf, table, &mut span_link.attributes)?,
                SpanLinkKey::TraceState => span_link.tracestate = read_string_ref(buf, table)?,
                SpanLinkKey::Flags => span_link.flags = read_number(buf)?,
            }
        } else {
            return Err(DecodeError::InvalidFormat("Invalid span link key".to_owned()))
        }
    }

    Ok(span_link)
}

fn decode_span_event<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<SpanEvent, DecodeError> {
    let mut span_event = SpanEvent::default();

    let span_event_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_event_size {
        let key = SpanEventKey::from_repr(read_key(buf)?);
        if let Some(key) = key {
            match key {
                SpanEventKey::Time => span_event.time_unix_nano = read_number(buf)?,
                SpanEventKey::Name => span_event.name = read_string_ref(buf, table)?,
                SpanEventKey::Attributes => decode_attributes(buf, table, &mut span_event.attributes)?,
            }
        } else {
            return Err(DecodeError::InvalidFormat("Invalid span event key".to_owned()))
        }
    }

    Ok(span_event)
}

fn decode_any_value<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>) -> Result<AttributeAnyValue, DecodeError> {
    Ok(match read_marker(buf.as_mut_slice()).map_err(|_| DecodeError::IOError)? {
        Marker::FixPos(value_type) => {
            match AnyValueKey::from_repr(value_type) {
                Some(AnyValueKey::String) => AttributeAnyValue::String(read_string_ref(buf, table)?),
                Some(AnyValueKey::Bool) => AttributeAnyValue::Boolean(decode::read_bool(buf.as_mut_slice()).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?),
                Some(AnyValueKey::Double) => AttributeAnyValue::Double(decode::read_f64(buf.as_mut_slice()).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?),
                Some(AnyValueKey::Int64) => AttributeAnyValue::Integer(read_number(buf)?),
                Some(AnyValueKey::Bytes) => AttributeAnyValue::Bytes(read_byte_ref(buf, table)?),
                Some(AnyValueKey::Array) => {
                    let array_len = decode::read_array_len(buf.as_mut_slice()).map_err(|_| DecodeError::InvalidFormat("Unable to get array len".to_owned()))?;
                    let mut array = Vec::with_capacity(array_len as usize);
                    for _ in 0..array_len {
                        array.push(decode_any_value(buf, table)?);
                    }
                    AttributeAnyValue::Array(array)
                }
                Some(AnyValueKey::Map) => {
                    let mut map = HashMap::new();
                    decode_attributes(buf, table, &mut map)?;
                    AttributeAnyValue::Map(map)
                }
                None => {
                    return Err(DecodeError::InvalidFormat("Invalid any value type".to_owned()));
                }
            }
        }
        _ => {
            return Err(DecodeError::InvalidFormat("Any value type is not FixPos".to_owned()));
        }
    })
}

fn decode_attributes<T: DeserializableTraceData>(buf: &mut Buffer<T>, table: &mut TraceStaticData<T>, map: &mut HashMap<TraceStringRef, AttributeAnyValue>) -> Result<(), DecodeError> {
    let attributes_size = decode::read_map_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for attributes size".to_owned())
    })?;
    for _ in 0..attributes_size {
        let key = read_string_ref(buf, table).map_err(|e| DecodeError::InvalidFormat(e.to_string()))?;
        let value = decode_any_value(buf, table)?;
        map.insert(key, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
}
