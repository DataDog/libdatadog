// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(super) mod span;

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v1::{TraceChunk, TracerPayload, TracerPayloadBytes, TracerPayloadSlice};
use crate::span::DeserializableTraceData;
use rmp::decode;
use std::borrow::Borrow;

// Integer keys used by the V1 wire format. Kept in sync with the encoder side
// (`msgpack_encoder::v1::{trace_key, chunk_key, SpanKey, SpanLinkKey, SpanEventKey, AnyValueKey}`).

pub(super) mod trace_key {
    pub const LANGUAGE_NAME: u8 = 3;
    pub const LANGUAGE_VERSION: u8 = 4;
    pub const TRACER_VERSION: u8 = 5;
    pub const RUNTIME_ID: u8 = 6;
    pub const ENV_REF: u8 = 7;
    pub const HOSTNAME_REF: u8 = 8;
    pub const APP_VERSION_REF: u8 = 9;
    pub const ATTRIBUTES: u8 = 10;
    pub const CHUNKS: u8 = 11;
}

pub(super) mod chunk_key {
    pub const PRIORITY: u8 = 1;
    pub const ORIGIN: u8 = 2;
    pub const ATTRIBUTES: u8 = 3;
    pub const SPANS: u8 = 4;
    pub const DROPPED_TRACE: u8 = 5;
    pub const TRACE_ID: u8 = 6;
    pub const SAMPLING_MECHANISM: u8 = 7;
}

pub(super) mod span_key {
    pub const SERVICE: u8 = 1;
    pub const NAME: u8 = 2;
    pub const RESOURCE: u8 = 3;
    pub const SPAN_ID: u8 = 4;
    pub const PARENT_ID: u8 = 5;
    pub const START: u8 = 6;
    pub const DURATION: u8 = 7;
    pub const ERROR: u8 = 8;
    pub const ATTRIBUTES: u8 = 9;
    pub const TYPE: u8 = 10;
    pub const SPAN_LINKS: u8 = 11;
    pub const SPAN_EVENTS: u8 = 12;
    pub const ENV: u8 = 13;
    pub const VERSION: u8 = 14;
    pub const COMPONENT: u8 = 15;
    pub const KIND: u8 = 16;
}

pub(super) mod span_link_key {
    pub const TRACE_ID: u8 = 1;
    pub const SPAN_ID: u8 = 2;
    pub const ATTRIBUTES: u8 = 3;
    pub const TRACE_STATE: u8 = 4;
    pub const FLAGS: u8 = 5;
}

pub(super) mod span_event_key {
    pub const TIME: u8 = 1;
    pub const NAME: u8 = 2;
    pub const ATTRIBUTES: u8 = 3;
}

pub(super) const ANY_VALUE_KEY_STRING: u8 = 1;
pub(super) const ANY_VALUE_KEY_BOOL: u8 = 2;
pub(super) const ANY_VALUE_KEY_DOUBLE: u8 = 3;
pub(super) const ANY_VALUE_KEY_INT64: u8 = 4;
pub(super) const ANY_VALUE_KEY_BYTES: u8 = 5;
pub(super) const ANY_VALUE_KEY_ARRAY: u8 = 6;
pub(super) const ANY_VALUE_KEY_KEY_VALUE_LIST: u8 = 7;

/// Number of msgpack items consumed per `[type, value]` pair in a typed `Array`.
pub(super) const TYPED_VALUE_STRIDE: u32 = 2;

/// Number of msgpack items consumed per `[key, type, value]` triplet in a typed attributes map.
pub(super) const FLAT_ATTR_STRIDE: u32 = 3;

/// Streaming string intern table built up as the payload is decoded.
///
/// V1 strings are encoded inline the first time they appear (as msgpack `str`), and as a
/// msgpack `uint` reference on every subsequent occurrence. ID 0 is reserved for the empty
/// string and is pre-inserted on construction.
pub(super) struct StringTable<T: DeserializableTraceData>
where
    T::Text: Clone,
{
    seen: Vec<T::Text>,
}

impl<T: DeserializableTraceData> StringTable<T>
where
    T::Text: Clone,
{
    pub(super) fn new() -> Self {
        Self {
            seen: vec![T::Text::default()],
        }
    }

    /// Resolves a string reference by ID (encoded inline as msgpack `uint`).
    fn resolve(&self, id: u64) -> Result<T::Text, DecodeError> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.seen.get(i).cloned())
            .ok_or_else(|| {
                DecodeError::InvalidFormat(format!(
                    "V1 string table reference out of range: id={id}, table_len={}",
                    self.seen.len()
                ))
            })
    }

    /// Records a freshly-read inline string and returns it (cloned for reuse).
    fn record(&mut self, s: T::Text) -> T::Text {
        self.seen.push(s.clone());
        s
    }
}

/// Reads a string-or-reference value at the current buffer position.
///
/// Decides based on the next msgpack marker:
/// - `str`/`fixstr` → read and intern, return the value
/// - any unsigned int marker → resolve the table reference
pub(super) fn read_interned_string<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<T::Text, DecodeError>
where
    T::Text: Clone,
{
    let slice: &[u8] = buf.as_mut_slice();
    let marker_byte = *slice.first().ok_or_else(|| {
        DecodeError::InvalidFormat(
            "Unexpected end of V1 buffer when reading interned string".to_owned(),
        )
    })?;

    // msgpack markers:
    //   fixstr           : 0xa0..=0xbf
    //   str8/str16/str32 : 0xd9, 0xda, 0xdb
    //   fixint (positive): 0x00..=0x7f
    //   uint8/16/32/64   : 0xcc, 0xcd, 0xce, 0xcf
    let is_string = matches!(marker_byte, 0xa0..=0xbf | 0xd9 | 0xda | 0xdb);
    let is_uint = matches!(marker_byte, 0x00..=0x7f | 0xcc | 0xcd | 0xce | 0xcf);

    if is_string {
        let s = buf.read_string()?;
        Ok(table.record(s))
    } else if is_uint {
        let id: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
            DecodeError::InvalidFormat("V1 interned string reference uint read failure".to_owned())
        })?;
        table.resolve(id)
    } else {
        Err(DecodeError::InvalidFormat(format!(
            "Unexpected msgpack marker 0x{marker_byte:02x} for V1 interned string"
        )))
    }
}

/// Decodes a V1 msgpack payload from owned bytes into a [`TracerPayloadBytes`].
///
/// # Returns
///
/// * `Ok((payload, payload_size))` — the decoded payload and the number of bytes consumed from the
///   buffer.
/// * `Err(DecodeError)` — if the payload is malformed.
///
/// # Errors
///
/// Returns an error for any malformed map / array length, unknown map key, missing required
/// field, or any embedded msgpack read failure.
pub fn from_bytes(
    data: libdd_tinybytes::Bytes,
) -> Result<(TracerPayloadBytes, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

/// Decodes a V1 msgpack payload from a borrowed slice into a [`TracerPayloadSlice`].
/// The resulting payload borrows from the input buffer (same lifetime).
pub fn from_slice(data: &[u8]) -> Result<(TracerPayloadSlice<'_>, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

/// Generic over the deserialization mode (owned `BytesData` or borrowed `SliceData`).
#[allow(clippy::type_complexity)]
pub fn from_buffer<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
) -> Result<(TracerPayload<T>, usize), DecodeError>
where
    T::Text: Clone,
{
    let start_len = data.len();
    let mut table = StringTable::<T>::new();
    let payload = decode_payload(data, &mut table)?;
    let consumed = start_len - data.len();
    Ok((payload, consumed))
}

/// Consumes and discards the msgpack value at the current buffer position, regardless of its
/// type. Used to skip unknown keys for forward compatibility: if the V1 format gains new fields,
/// older decoders shouldn't reject the whole payload just because they don't recognize a key.
pub(super) fn skip_unknown_value<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<(), DecodeError> {
    rmpv::decode::read_value(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Failed to skip unknown V1 value".to_owned()))?;
    Ok(())
}

/// Decodes the top-level V1 payload map: tracer metadata fields + chunks array.
fn decode_payload<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<TracerPayload<T>, DecodeError>
where
    T::Text: Clone,
{
    let map_len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read V1 payload map len".to_owned()))?;

    let mut payload = TracerPayload::<T>::default();
    let mut saw_chunks = false;

    for _ in 0..map_len {
        let key = decode::read_int::<u8, _>(buf.as_mut_slice()).map_err(|_| {
            DecodeError::InvalidFormat("V1 payload key (u8) read failure".to_owned())
        })?;
        match key {
            trace_key::CHUNKS => {
                payload.chunks = decode_chunks(buf, table)?;
                saw_chunks = true;
            }
            trace_key::LANGUAGE_NAME => payload.language_name = read_interned_string(buf, table)?,
            trace_key::LANGUAGE_VERSION => {
                payload.language_version = read_interned_string(buf, table)?
            }
            trace_key::TRACER_VERSION => payload.tracer_version = read_interned_string(buf, table)?,
            trace_key::RUNTIME_ID => payload.runtime_id = read_interned_string(buf, table)?,
            trace_key::ENV_REF => payload.env = read_interned_string(buf, table)?,
            trace_key::HOSTNAME_REF => payload.hostname = read_interned_string(buf, table)?,
            trace_key::APP_VERSION_REF => payload.app_version = read_interned_string(buf, table)?,
            trace_key::ATTRIBUTES => {
                payload.attributes = span::read_attributes_map(buf, table)?;
            }
            _unknown => skip_unknown_value(buf)?,
        }
    }

    if !saw_chunks {
        return Err(DecodeError::InvalidFormat(
            "V1 payload is missing the chunks field".to_owned(),
        ));
    }

    Ok(payload)
}

fn decode_chunks<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<Vec<TraceChunk<T>>, DecodeError>
where
    T::Text: Clone,
{
    let count = decode::read_array_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 chunks array len read failure".to_owned()))?;
    let mut chunks = Vec::with_capacity(count as usize);
    for _ in 0..count {
        chunks.push(decode_chunk(buf, table)?);
    }
    Ok(chunks)
}

fn decode_chunk<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<TraceChunk<T>, DecodeError>
where
    T::Text: Clone,
{
    let map_len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 chunk map len read failure".to_owned()))?;
    let mut chunk = TraceChunk::<T>::default();
    let mut saw_trace_id = false;
    let mut saw_spans = false;

    for _ in 0..map_len {
        let key = decode::read_int::<u8, _>(buf.as_mut_slice())
            .map_err(|_| DecodeError::InvalidFormat("V1 chunk key (u8) read failure".to_owned()))?;
        match key {
            chunk_key::TRACE_ID => {
                let len = decode::read_bin_len(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 chunk trace_id bin len read failure".to_owned())
                })?;
                if len != 16 {
                    return Err(DecodeError::InvalidFormat(format!(
                        "V1 chunk trace_id must be 16 bytes, got {len}"
                    )));
                }
                let bytes = buf.try_slice_and_advance(16).ok_or_else(|| {
                    DecodeError::InvalidFormat("V1 chunk trace_id payload truncated".to_owned())
                })?;
                let slice: &[u8] = bytes.borrow();
                chunk.trace_id.copy_from_slice(slice);
                saw_trace_id = true;
            }
            chunk_key::SPANS => {
                let count = decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 chunk spans array len read failure".to_owned())
                })?;
                let mut spans = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    spans.push(span::decode_span(buf, table)?);
                }
                chunk.spans = spans;
                saw_spans = true;
            }
            chunk_key::ORIGIN => chunk.origin = read_interned_string(buf, table)?,
            chunk_key::PRIORITY => {
                let v: i64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat("V1 chunk priority read failure".to_owned())
                })?;
                chunk.priority = Some(v as i32);
            }
            chunk_key::SAMPLING_MECHANISM => {
                let v: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat(
                        "V1 chunk sampling_mechanism read failure".to_owned(),
                    )
                })?;
                chunk.sampling_mechanism = Some(v as u32);
            }
            chunk_key::ATTRIBUTES => {
                chunk.attributes = span::read_attributes_map(buf, table)?;
            }
            chunk_key::DROPPED_TRACE => {
                chunk.dropped_trace = decode::read_bool(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat(
                        "V1 chunk dropped_trace bool read failure".to_owned(),
                    )
                })?;
            }
            _unknown => skip_unknown_value(buf)?,
        }
    }

    if !saw_trace_id {
        return Err(DecodeError::InvalidFormat(
            "V1 chunk is missing trace_id".to_owned(),
        ));
    }
    if !saw_spans {
        return Err(DecodeError::InvalidFormat(
            "V1 chunk is missing spans array".to_owned(),
        ));
    }

    Ok(chunk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_encoder::v1::to_vec_from_payload_v1;
    use crate::span::v1::{
        AttributeValue, Span as V1Span, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes,
        TracerPayloadBytes,
    };
    use crate::span::vec_map::VecMap;
    use bolero::check;
    use libdd_tinybytes::{Bytes, BytesString};

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
    }

    fn sample_payload() -> TracerPayloadBytes {
        let mut attrs = VecMap::<BytesString, AttributeValue<_>>::new();
        attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        attrs.insert(bs("http.status"), AttributeValue::Int(200));
        attrs.insert(bs("is_root"), AttributeValue::Bool(true));
        attrs.insert(bs("ratio"), AttributeValue::Float(0.75));
        attrs.insert(
            bs("ids"),
            AttributeValue::List(vec![AttributeValue::Int(1), AttributeValue::Int(2)]),
        );

        let span = V1Span {
            service: bs("svc"),
            name: bs("GET /users"),
            resource: bs("/users"),
            r#type: bs("web"),
            span_id: 42,
            parent_id: 7,
            start: 1_700_000_000_000,
            duration: 1_500,
            error: true,
            span_kind: SpanKind::Server,
            env: bs("prod"),
            version: bs("1.2.3"),
            component: bs("net/http"),
            attributes: attrs,
            ..Default::default()
        };

        let mut chunk_attrs = VecMap::<BytesString, AttributeValue<_>>::new();
        chunk_attrs.insert(bs("_dd.p.dm"), AttributeValue::String(bs("-1")));

        let chunk = TraceChunkBytes {
            trace_id: [1u8; 16],
            priority: Some(1),
            origin: bs("synthetic"),
            sampling_mechanism: Some(2),
            dropped_trace: false,
            attributes: chunk_attrs,
            spans: vec![span],
        };

        TracerPayloadBytes {
            language_name: bs("rust"),
            language_version: bs("1.87"),
            tracer_version: bs("9.9.9"),
            runtime_id: bs("abcd-1234"),
            env: bs("prod"),
            hostname: bs("host-1"),
            app_version: bs("1.2.3"),
            chunks: vec![chunk],
            ..Default::default()
        }
    }

    #[test]
    fn roundtrip_full_payload() {
        let original = sample_payload();
        let bytes = to_vec_from_payload_v1(&original);
        let payload_len = bytes.len();
        let (decoded, consumed) =
            from_bytes(Bytes::from(bytes)).expect("decoder should succeed on encoder output");

        assert_eq!(consumed, payload_len, "decoder should consume all bytes");

        // Tracer-level metadata
        assert_eq!(decoded.language_name.as_str(), "rust");
        assert_eq!(decoded.language_version.as_str(), "1.87");
        assert_eq!(decoded.tracer_version.as_str(), "9.9.9");
        assert_eq!(decoded.runtime_id.as_str(), "abcd-1234");
        assert_eq!(decoded.env.as_str(), "prod");
        assert_eq!(decoded.hostname.as_str(), "host-1");
        assert_eq!(decoded.app_version.as_str(), "1.2.3");

        // Chunk
        assert_eq!(decoded.chunks.len(), 1);
        let chunk = &decoded.chunks[0];
        assert_eq!(chunk.trace_id, [1u8; 16]);
        assert_eq!(chunk.priority, Some(1));
        assert_eq!(chunk.sampling_mechanism, Some(2));
        assert_eq!(chunk.origin.as_str(), "synthetic");
        assert_eq!(chunk.attributes.len(), 1);

        // Span
        assert_eq!(chunk.spans.len(), 1);
        let span = &chunk.spans[0];
        assert_eq!(span.service.as_str(), "svc");
        assert_eq!(span.name.as_str(), "GET /users");
        assert_eq!(span.resource.as_str(), "/users");
        assert_eq!(span.r#type.as_str(), "web");
        assert_eq!(span.span_id, 42);
        assert_eq!(span.parent_id, 7);
        assert_eq!(span.start, 1_700_000_000_000);
        assert_eq!(span.duration, 1_500);
        assert!(span.error);
        assert_eq!(span.span_kind, SpanKind::Server);
        assert_eq!(span.env.as_str(), "prod");
        assert_eq!(span.version.as_str(), "1.2.3");
        assert_eq!(span.component.as_str(), "net/http");
        assert_eq!(span.attributes.len(), 5);
    }

    #[test]
    fn empty_payload_roundtrip() {
        let original = TracerPayloadBytes::default();
        let bytes = to_vec_from_payload_v1(&original);
        let (decoded, _) =
            from_bytes(Bytes::from(bytes)).expect("decoder should succeed on empty payload");
        assert!(decoded.chunks.is_empty());
        assert!(decoded.language_name.as_str().is_empty());
    }

    #[test]
    fn missing_chunks_field_is_rejected() {
        // Manually encode a payload map with only one entry (env), no chunks field.
        // `0x81` = fixmap len 1, key 0x07 (ENV_REF), value = inline str "x" (`0xa1 0x78`).
        let bytes = vec![0x81, 0x07, 0xa1, 0x78];
        let err = from_bytes(Bytes::from(bytes)).expect_err("missing chunks must error");
        assert!(matches!(err, DecodeError::InvalidFormat(_)));
    }

    #[test]
    fn truncated_trace_id_is_rejected_not_panicking() {
        // Payload map with 1 entry: chunks -> [ chunk map with 1 entry: trace_id -> bin(16) ].
        // The bin declares 16 bytes but only 4 are actually present, so the owned decoder's
        // `try_slice_and_advance` must reject this instead of indexing out of bounds.
        let bytes = vec![
            0x81,
            trace_key::CHUNKS,
            0x91, // array len 1
            0x81, // chunk fixmap len 1
            chunk_key::TRACE_ID,
            0xc4, // bin8 marker
            0x10, // declared length: 16 bytes
            0x01,
            0x02,
            0x03,
            0x04, // only 4 bytes actually present
        ];
        let err = from_bytes(Bytes::from(bytes)).expect_err("truncated trace_id must error");
        assert!(matches!(err, DecodeError::InvalidFormat(_)));
    }

    #[test]
    fn string_interning_resolves_across_chunks() {
        // Two chunks sharing the same service name. The decoded service strings must both
        // be "shared" — verifying the streaming string table is preserved across chunks.
        let span_a = V1Span {
            service: bs("shared"),
            name: bs("a"),
            span_id: 1,
            start: 1,
            ..Default::default()
        };
        let span_b = V1Span {
            service: bs("shared"),
            name: bs("b"),
            span_id: 2,
            start: 1,
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![
                TraceChunkBytes {
                    trace_id: [1u8; 16],
                    spans: vec![span_a],
                    ..Default::default()
                },
                TraceChunkBytes {
                    trace_id: [2u8; 16],
                    spans: vec![span_b],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let bytes = to_vec_from_payload_v1(&payload);
        let (decoded, _) =
            from_bytes(Bytes::from(bytes)).expect("decoder should resolve interned strings");
        assert_eq!(decoded.chunks[0].spans[0].service.as_str(), "shared");
        assert_eq!(decoded.chunks[1].spans[0].service.as_str(), "shared");
    }

    #[test]
    fn nested_keyvalue_attribute_roundtrip() {
        let mut inner = VecMap::<BytesString, AttributeValue<_>>::new();
        inner.insert(bs("k"), AttributeValue::String(bs("v")));
        let mut attrs = VecMap::<BytesString, AttributeValue<_>>::new();
        attrs.insert(bs("nested"), AttributeValue::KeyValue(inner));

        let span = V1Span {
            service: bs("svc"),
            name: bs("op"),
            span_id: 1,
            start: 1,
            attributes: attrs,
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: [0u8; 16],
                spans: vec![span],
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = to_vec_from_payload_v1(&payload);
        let (decoded, _) = from_bytes(Bytes::from(bytes)).expect("nested KeyValue roundtrip");

        let decoded_attrs = &decoded.chunks[0].spans[0].attributes;
        match decoded_attrs.get(&bs("nested")) {
            Some(AttributeValue::KeyValue(map)) => {
                assert_eq!(map.len(), 1);
                match map.get(&bs("k")) {
                    Some(AttributeValue::String(v)) => assert_eq!(v.as_str(), "v"),
                    _ => panic!("inner value should be String"),
                }
            }
            _ => panic!("attribute should decode as KeyValue"),
        }
    }

    /// Fuzz test: bolero generates random strings + numbers for the V1 payload, the encoder
    /// serialises it, and the decoder must accept its own output (no panic, no error). Mirrors
    /// the v04 `fuzz_from_bytes` pattern. Bolero caps tuples at 12 fields — extra metadata is
    /// either omitted or filled with deterministic defaults.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn fuzz_from_bytes() {
        check!()
            .with_type::<(
                String, // language_name
                String, // env (payload-level)
                String, // service
                String, // name
                String, // resource
                String, // span env
                String, // attr_key
                String, // attr_value
                u64,    // span_id
                u64,    // parent_id
                u64,    // start
                bool,   // error
            )>()
            .cloned()
            .for_each(
                |(
                    lang,
                    payload_env,
                    service,
                    name,
                    resource,
                    span_env,
                    attr_key,
                    attr_value,
                    span_id,
                    parent_id,
                    start,
                    error,
                )| {
                    let bs = |s: &str| BytesString::from_slice(s.as_ref()).unwrap();
                    let mut attrs = VecMap::<BytesString, AttributeValue<_>>::new();
                    attrs.insert(bs(&attr_key), AttributeValue::String(bs(&attr_value)));

                    let span = V1SpanBytes {
                        service: bs(&service),
                        name: bs(&name),
                        resource: bs(&resource),
                        span_id,
                        parent_id,
                        start: start as i64,
                        error,
                        env: bs(&span_env),
                        attributes: attrs,
                        ..Default::default()
                    };

                    let payload = TracerPayloadBytes {
                        language_name: bs(&lang),
                        env: bs(&payload_env),
                        chunks: vec![TraceChunkBytes {
                            trace_id: [0xab; 16],
                            spans: vec![span],
                            ..Default::default()
                        }],
                        ..Default::default()
                    };

                    let encoded = to_vec_from_payload_v1(&payload);
                    let result = from_bytes(Bytes::from(encoded));
                    assert!(
                        result.is_ok(),
                        "decoder rejected its own encoded output: {result:?}"
                    );
                },
            );
    }
}
