// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::{read_nullable_number, read_num, read_number};
use crate::span::{v1::Span, DeserializableTraceData};
use hashbrown::HashMap;
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
        Ok(table.add_bytes(data))
    } else {
        Err(DecodeError::InvalidFormat(
            "Invalid data length".to_string(),
        ))
    }
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
    use hashbrown::HashMap;
    use libdd_tinybytes::Bytes;
    use libdd_trace_protobuf::pb::idx::SpanKind;
    use crate::msgpack_encoder;
    use crate::msgpack_decoder;
    use crate::span::{BytesData, v1::{
        AttributeAnyValue, Span, SpanEvent, SpanLink, TraceChunk, TracePayload,
    }};
    use crate::span::v1::to_v04;

    fn roundtrip(payload: &TracePayload<BytesData>) -> TracePayload<BytesData> {
        let bytes = Bytes::from(msgpack_encoder::v1::to_vec(payload));
        msgpack_decoder::v1::from_bytes(bytes)
            .expect("Decoding failed")
            .0
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// A completely default payload with no data should survive the round-trip.
    #[test]
    fn test_roundtrip_empty_payload() {
        let payload = TracePayload::<BytesData>::default();
        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// All trace-level string fields (no chunks) must survive the round-trip.
    #[test]
    fn test_roundtrip_trace_level_fields() {
        let mut payload = TracePayload::<BytesData>::default();
        payload.traces.container_id = payload.static_data.add_string("container-abc123");
        payload.traces.language_name = payload.static_data.add_string("php");
        payload.traces.language_version = payload.static_data.add_string("8.3.0");
        payload.traces.tracer_version = payload.static_data.add_string("1.2.3");
        payload.traces.runtime_id = payload.static_data.add_string("runtime-uuid-xyz");
        payload.traces.env = payload.static_data.add_string("production");
        payload.traces.hostname = payload.static_data.add_string("web-server-01");
        payload.traces.app_version = payload.static_data.add_string("v2.5.1");

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// Trace-level attributes of primitive types (string, bool, int, double)
    /// must survive the round-trip.
    #[test]
    fn test_roundtrip_trace_attributes_primitives() {
        let mut payload = TracePayload::<BytesData>::default();

        let k1 = payload.static_data.add_string("str_attr");
        let v1 = AttributeAnyValue::String(payload.static_data.add_string("hello world"));
        let k2 = payload.static_data.add_string("bool_attr");
        let v2 = AttributeAnyValue::Boolean(true);
        let k3 = payload.static_data.add_string("int_attr");
        let v3 = AttributeAnyValue::Integer(-42);
        let k4 = payload.static_data.add_string("double_attr");
        let v4 = AttributeAnyValue::Double(3.14159_f64);

        payload.traces.attributes.insert(k1, v1);
        payload.traces.attributes.insert(k2, v2);
        payload.traces.attributes.insert(k3, v3);
        payload.traces.attributes.insert(k4, v4);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// Bytes-typed attributes must survive the round-trip.
    #[test]
    fn test_roundtrip_trace_attributes_bytes() {
        let mut payload = TracePayload::<BytesData>::default();

        let k = payload.static_data.add_string("bytes_attr");
        let bytes_ref = payload.static_data.add_bytes(Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]));
        let v = AttributeAnyValue::Bytes(bytes_ref);
        payload.traces.attributes.insert(k, v);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// Nested attribute values (arrays and maps) must survive the round-trip.
    #[test]
    fn test_roundtrip_nested_attributes() {
        let mut payload = TracePayload::<BytesData>::default();

        // Array value containing mixed types
        let arr_key = payload.static_data.add_string("array_attr");
        let arr_val = AttributeAnyValue::Array(vec![
            AttributeAnyValue::String(payload.static_data.add_string("item1")),
            AttributeAnyValue::Integer(99),
            AttributeAnyValue::Boolean(false),
            AttributeAnyValue::Double(2.718281828_f64),
        ]);
        payload.traces.attributes.insert(arr_key, arr_val);

        // Map value
        let map_key = payload.static_data.add_string("map_attr");
        let inner_key = payload.static_data.add_string("nested_key");
        let inner_val = AttributeAnyValue::String(payload.static_data.add_string("nested_val"));
        let mut inner_map = HashMap::new();
        inner_map.insert(inner_key, inner_val);
        payload.traces.attributes.insert(map_key, AttributeAnyValue::Map(inner_map));

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// All chunk-level scalar fields (priority, origin, dropped_trace, trace_id,
    /// sampling_mechanism) must survive the round-trip.
    #[test]
    fn test_roundtrip_chunk_fields() {
        let mut payload = TracePayload::<BytesData>::default();

        let mut chunk = TraceChunk::default();
        chunk.priority = 2;
        chunk.origin = payload.static_data.add_string("rum");
        chunk.dropped_trace = true;
        chunk.trace_id = 0x1234567890abcdef_1234567890abcdef_u128;
        chunk.sampling_mechanism = 3;
        payload.traces.chunks.push(chunk);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// Attributes attached to a chunk must survive the round-trip.
    #[test]
    fn test_roundtrip_chunk_attributes() {
        let mut payload = TracePayload::<BytesData>::default();

        let k = payload.static_data.add_string("chunk.info");
        let v = AttributeAnyValue::Integer(42);
        let mut chunk = TraceChunk::default();
        chunk.trace_id = 1;
        chunk.attributes.insert(k, v);
        payload.traces.chunks.push(chunk);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// A fully populated span (all fields, all attribute types, non-default kind)
    /// must survive the round-trip.
    #[test]
    fn test_roundtrip_span_all_fields() {
        let mut payload = TracePayload::<BytesData>::default();

        let mut span = Span::default();
        span.service = payload.static_data.add_string("my-service");
        span.name = payload.static_data.add_string("GET /api/users");
        span.resource = payload.static_data.add_string("/api/users");
        span.r#type = payload.static_data.add_string("http");
        span.env = payload.static_data.add_string("staging");
        span.version = payload.static_data.add_string("1.0.0");
        span.component = payload.static_data.add_string("web");
        span.span_id = 0xabcdef1234567890_u64;
        span.parent_id = 0x1234567890abcdef_u64;
        span.start = 1_700_000_000_000_000_000_i64;
        span.duration = 5_000_000_i64;
        span.error = true;
        span.kind = SpanKind::Server;

        let ak = payload.static_data.add_string("http.status_code");
        span.attributes.insert(ak, AttributeAnyValue::Integer(200));

        let mut chunk = TraceChunk::default();
        chunk.trace_id = 0xfedcba9876543210_fedcba9876543210_u128;
        chunk.spans.push(span);
        payload.traces.chunks.push(chunk);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// SpanLinks (with all fields and attributes) must survive the round-trip.
    #[test]
    fn test_roundtrip_span_links() {
        let mut payload = TracePayload::<BytesData>::default();

        let tracestate = payload.static_data.add_string("dd=s:1,t.tid:abc123");
        let lk = payload.static_data.add_string("link.kind");
        let lv = AttributeAnyValue::String(payload.static_data.add_string("follows_from"));

        let mut link = SpanLink::default();
        link.trace_id = 0x11223344_55667788_99aabbcc_ddeeff00_u128;
        link.span_id = 0xdeadbeef12345678_u64;
        link.tracestate = tracestate;
        link.flags = 1;
        link.attributes.insert(lk, lv);

        let mut span = Span::default();
        span.span_id = 1;
        span.start = 1000;
        span.span_links.push(link);

        let mut chunk = TraceChunk::default();
        chunk.trace_id = 42;
        chunk.spans.push(span);
        payload.traces.chunks.push(chunk);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// SpanEvents (with all fields and attributes) must survive the round-trip.
    #[test]
    fn test_roundtrip_span_events() {
        let mut payload = TracePayload::<BytesData>::default();

        let event_name = payload.static_data.add_string("exception");
        let ek = payload.static_data.add_string("exception.message");
        let ev = AttributeAnyValue::String(
            payload.static_data.add_string("null pointer dereference"),
        );

        let mut event = SpanEvent::default();
        event.time_unix_nano = 1_700_000_001_000_000_000_u64;
        event.name = event_name;
        event.attributes.insert(ek, ev);

        let mut span = Span::default();
        span.span_id = 2;
        span.start = 1000;
        span.span_events.push(event);

        let mut chunk = TraceChunk::default();
        chunk.trace_id = 99;
        chunk.spans.push(span);
        payload.traces.chunks.push(chunk);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// The same string ref used in multiple spans/chunks must decode to the
    /// same string value everywhere (tests encoder deduplication + decoder
    /// index-based re-use).
    #[test]
    fn test_roundtrip_string_deduplication() {
        let mut payload = TracePayload::<BytesData>::default();

        // Assign the same interned string ref to multiple fields
        let shared_service = payload.static_data.add_string("shared-service");
        payload.traces.language_name = payload.static_data.add_string("php");

        // chunk1: origin reuses the same "php" string as language_name
        let origin = payload.static_data.add_string("php");

        let mut chunk1 = TraceChunk::default();
        chunk1.trace_id = 1;
        chunk1.origin = origin;
        let mut span1 = Span::default();
        span1.service = shared_service;
        span1.name = payload.static_data.add_string("op-a");
        span1.span_id = 100;
        span1.start = 1000;
        chunk1.spans.push(span1);
        payload.traces.chunks.push(chunk1);

        // chunk2: reuses shared_service ref (second occurrence → encoded as index)
        let mut chunk2 = TraceChunk::default();
        chunk2.trace_id = 2;
        let mut span2 = Span::default();
        span2.service = shared_service;
        span2.name = payload.static_data.add_string("op-b");
        span2.span_id = 200;
        span2.start = 2000;
        chunk2.spans.push(span2);
        payload.traces.chunks.push(chunk2);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }

    /// A maximally complex payload exercises the full encoder/decoder path:
    /// multiple chunks, multiple spans, span links, span events, attributes of
    /// every supported type, and repeated strings.
    #[test]
    fn test_roundtrip_fully_populated() {
        let mut payload = TracePayload::<BytesData>::default();

        // Trace-level fields
        payload.traces.container_id = payload.static_data.add_string("container-deadbeef");
        payload.traces.language_name = payload.static_data.add_string("java");
        payload.traces.language_version = payload.static_data.add_string("21.0.1");
        payload.traces.tracer_version = payload.static_data.add_string("1.30.0");
        payload.traces.runtime_id =
            payload.static_data.add_string("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx");
        payload.traces.env = payload.static_data.add_string("prod");
        payload.traces.hostname = payload.static_data.add_string("web-01.example.com");
        payload.traces.app_version = payload.static_data.add_string("2.0.0");

        let ta_k = payload.static_data.add_string("trace.global");
        let ta_v = AttributeAnyValue::String(payload.static_data.add_string("global-value"));
        payload.traces.attributes.insert(ta_k, ta_v);

        // Shared string reused across spans
        let service = payload.static_data.add_string("order-service");

        // ── Chunk 1 ──────────────────────────────────────────────────────────
        let ca_k = payload.static_data.add_string("chunk.meta");
        let ca_v = AttributeAnyValue::Integer(7);

        let mut chunk1 = TraceChunk::default();
        chunk1.priority = 1;
        chunk1.origin = payload.static_data.add_string("rum");
        chunk1.trace_id = 0xabcdef12_34567890_abcdef12_34567890_u128;
        chunk1.sampling_mechanism = 2;
        chunk1.attributes.insert(ca_k, ca_v);

        // Span 1: all attribute types + span link + span event
        let mut span1 = Span::default();
        span1.service = service;
        span1.name = payload.static_data.add_string("create-order");
        span1.resource = payload.static_data.add_string("POST /orders");
        span1.r#type = payload.static_data.add_string("web");
        span1.env = payload.static_data.add_string("prod");
        span1.version = payload.static_data.add_string("2.0.0");
        span1.component = payload.static_data.add_string("orders");
        span1.span_id = 0x1111111111111111_u64;
        span1.start = 1_700_000_001_000_000_000_i64;
        span1.duration = 10_000_000_i64;
        span1.error = false;
        span1.kind = SpanKind::Server;

        let a1k = payload.static_data.add_string("http.method");
        span1.attributes.insert(
            a1k,
            AttributeAnyValue::String(payload.static_data.add_string("POST")),
        );
        let a2k = payload.static_data.add_string("http.status");
        span1.attributes.insert(a2k, AttributeAnyValue::Integer(201));
        let a3k = payload.static_data.add_string("success");
        span1.attributes.insert(a3k, AttributeAnyValue::Boolean(true));
        let a4k = payload.static_data.add_string("latency_ms");
        span1.attributes.insert(a4k, AttributeAnyValue::Double(12.5_f64));
        let a5k = payload.static_data.add_string("tags");
        let tag_a = AttributeAnyValue::String(payload.static_data.add_string("tag_a"));
        let tag_b = AttributeAnyValue::String(payload.static_data.add_string("tag_b"));
        span1.attributes.insert(a5k, AttributeAnyValue::Array(vec![tag_a, tag_b]));
        let a6k = payload.static_data.add_string("meta");
        let mk = payload.static_data.add_string("region");
        let mv = AttributeAnyValue::String(payload.static_data.add_string("us-east-1"));
        let mut meta = HashMap::new();
        meta.insert(mk, mv);
        span1.attributes.insert(a6k, AttributeAnyValue::Map(meta));

        // SpanLink on span1
        let lk = payload.static_data.add_string("link.type");
        let lv = AttributeAnyValue::String(payload.static_data.add_string("follows_from"));
        let mut link = SpanLink::default();
        link.trace_id = 0x11111111_11111111_22222222_22222222_u128;
        link.span_id = 0xaaaaaaaabbbbbbbb_u64;
        link.tracestate = payload.static_data.add_string("vendor=val");
        link.flags = 1;
        link.attributes.insert(lk, lv);
        span1.span_links.push(link);

        // SpanEvent on span1
        let ek = payload.static_data.add_string("db.statement");
        let ev = AttributeAnyValue::String(payload.static_data.add_string("SELECT 1"));
        let mut event = SpanEvent::default();
        event.time_unix_nano = 1_700_000_001_500_000_000_u64;
        event.name = payload.static_data.add_string("db.query");
        event.attributes.insert(ek, ev);
        span1.span_events.push(event);
        chunk1.spans.push(span1);

        // Span 2: reuses service string (tests second-occurrence deduplication)
        let mut span2 = Span::default();
        span2.service = service;
        span2.name = payload.static_data.add_string("validate");
        span2.span_id = 0x2222222222222222_u64;
        span2.parent_id = 0x1111111111111111_u64;
        span2.start = 1_700_000_001_100_000_000_i64;
        span2.duration = 1_000_000_i64;
        span2.error = true;
        chunk1.spans.push(span2);
        payload.traces.chunks.push(chunk1);

        // ── Chunk 2 ──────────────────────────────────────────────────────────
        let mut chunk2 = TraceChunk::default();
        chunk2.trace_id = 0x99999999_99999999_aaaaaaaa_aaaaaaaa_u128;
        chunk2.dropped_trace = true;
        let mut span3 = Span::default();
        span3.service = service; // third occurrence of "order-service"
        span3.name = payload.static_data.add_string("background-job");
        span3.span_id = 0x3333333333333333_u64;
        span3.start = 1_700_000_002_000_000_000_i64;
        chunk2.spans.push(span3);
        payload.traces.chunks.push(chunk2);

        let decoded = roundtrip(&payload);
        assert_eq!(to_v04(&payload), to_v04(&decoded));
    }
}
