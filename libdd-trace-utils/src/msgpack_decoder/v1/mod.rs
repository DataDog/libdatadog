// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(super) mod span;

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::v1::{TraceChunk, TracerPayload, TracerPayloadBytes, TracerPayloadSlice};
use crate::span::{DeserializableTraceData, SpanText};
use rmp::decode;
use std::borrow::Borrow;

// Integer keys used by the V1 wire format. Kept in sync with the encoder side
// (`msgpack_encoder::v1::{trace_key, chunk_key, SpanKey, SpanLinkKey, SpanEventKey, AnyValueKey}`).

pub(super) mod trace_key {
    pub const CONTAINER_ID: u8 = 2;
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

/// Length in bytes of a V1 chunk's `trace_id` field (128-bit trace ID).
pub(super) const TRACE_ID_LEN: u32 = 16;

/// Streaming string intern table built up as the payload is decoded.
///
/// V1 strings are encoded inline the first time they appear (as msgpack `str`), and as a
/// msgpack `uint` reference on every subsequent occurrence. ID 0 is reserved for the empty
/// string and is pre-inserted on construction.
///
/// Every inline string takes the next wire index, even when its value equals an earlier entry,
/// so references stay aligned with the producer's own table. Two entries can hold the same value
/// when invalid UTF-8 is repaired (`"user\x80"` and `"user\x81"` both become `"user\u{FFFD}"`).
/// The decoded model keys attributes by string value, not by wire index, so equal values are
/// already one attribute key: `VecMap` lookups match by value and re-encoding dedups them
/// last-write-wins. Unlike the Agent, which stores attributes by string index and has to map a
/// repeated value's later wire index back to its first index, no aliasing is needed here.
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

    /// Records a freshly-read inline string in the table.
    fn record(&mut self, s: &T::Text) {
        self.seen.push(s.clone());
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
    let slice: &[u8] = buf.as_slice();
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
    match marker_byte {
        0xa0..=0xbf | 0xd9 | 0xda | 0xdb => {
            let s = match buf.read_string() {
                Ok(s) => s,
                // `read_string` leaves the buffer in place on a UTF-8 failure.
                Err(DecodeError::Utf8Error(_)) => read_string_lossy(buf)?,
                Err(e) => return Err(e),
            };
            table.record(&s);
            Ok(s)
        }
        0x00..=0x7f | 0xcc | 0xcd | 0xce | 0xcf => {
            let id: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                DecodeError::InvalidFormat(
                    "V1 interned string reference uint read failure".to_owned(),
                )
            })?;
            table.resolve(id)
        }
        _ => Err(DecodeError::InvalidFormat(format!(
            "Unexpected msgpack marker 0x{marker_byte:02x} for V1 interned string"
        ))),
    }
}

/// Reads the msgpack `str` at the current buffer position, repairing invalid UTF-8 with U+FFFD
/// (see [`lossy_text`]).
///
/// This is the slow path for strings that failed UTF-8 validation: the repaired value no longer
/// matches the wire bytes, so it is copied into an owned allocation instead of borrowing from the
/// buffer. Valid strings never come here.
fn read_string_lossy<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<T::Text, DecodeError> {
    let len = decode::read_str_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("V1 string len read failure".to_owned()))?;
    let len = usize::try_from(len).map_err(|_| {
        DecodeError::InvalidFormat(format!("V1 string len {len} exceeds usize::MAX"))
    })?;
    let raw = buf
        .try_slice_and_advance(len)
        .ok_or_else(|| DecodeError::InvalidFormat("V1 string payload truncated".to_owned()))?;
    Ok(lossy_text::<T>(raw.borrow()))
}

/// Builds an owned text from `bytes`, repairing invalid UTF-8 the way the Agent's V1 and v0.4
/// decoders do: every byte that is not part of a valid sequence becomes one U+FFFD, since Go's
/// rune decoding advances a single byte on an error. [`String::from_utf8_lossy`] would instead
/// collapse a truncated multi-byte sequence into one U+FFFD; matching the Agent keeps a repaired
/// string identical whichever of the two decoded it.
fn lossy_text<T: DeserializableTraceData>(bytes: &[u8]) -> T::Text {
    // Each invalid byte grows to the 3-byte U+FFFD, so this may still reallocate once.
    let mut repaired = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        repaired.push_str(chunk.valid());
        for _ in chunk.invalid() {
            repaired.push(char::REPLACEMENT_CHARACTER);
        }
    }
    T::Text::from_owned(repaired)
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
///
/// Any inline string encountered while skipping (at any nesting depth) is interned into `table`,
/// same as a recognized field would: skipping a value must not desync later back-references to
/// strings that happen to also appear inside it.
pub(super) fn skip_unknown_value<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    table: &mut StringTable<T>,
) -> Result<(), DecodeError>
where
    T::Text: Clone,
{
    // Snapshot the buffer's owning handle *before* advancing past the skipped value: any string
    // found inside it will be a substring of this exact allocation, so this is what
    // `T::intern_skipped_str` must derive ownership from. Cloning is cheap (a refcount bump for
    // `T::Bytes = Bytes`), unaffected by the lied `'static` lifetime `as_mut_slice` exposes.
    let owner = buf.bytes().clone();
    let value = rmpv::decode::read_value_ref(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Failed to skip unknown V1 value".to_owned()))?;
    record_strings_in_value_ref::<T>(&value, &owner, table);
    Ok(())
}

/// Recursively walks a parsed [`rmpv::ValueRef`], interning every string it contains into
/// `table`. A producer assigns a wire index to every inline string, whatever its field, so each
/// one must take a table entry here too, or later references would point one entry off. Strings
/// with invalid UTF-8 are repaired with U+FFFD, the same as [`read_interned_string`] does.
///
/// `owner` must be a snapshot of the buffer taken before it was advanced past `value`: the
/// strings inside `value` report a lied `'static` lifetime (see `Buffer::as_mut_slice`) but
/// really borrow from `owner`'s memory.
fn record_strings_in_value_ref<T: DeserializableTraceData>(
    value: &rmpv::ValueRef<'static>,
    owner: &T::Bytes,
    table: &mut StringTable<T>,
) where
    T::Text: Clone,
{
    match value {
        rmpv::ValueRef::String(s) => {
            let text = match (*s).into_str() {
                Some(s) => T::intern_skipped_str(owner, s),
                None => lossy_text::<T>(s.as_bytes()),
            };
            table.record(&text);
        }
        rmpv::ValueRef::Array(items) => {
            for item in items {
                record_strings_in_value_ref::<T>(item, owner, table);
            }
        }
        rmpv::ValueRef::Map(entries) => {
            for (key, val) in entries {
                record_strings_in_value_ref::<T>(key, owner, table);
                record_strings_in_value_ref::<T>(val, owner, table);
            }
        }
        _ => {}
    }
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
            trace_key::CONTAINER_ID => payload.container_id = read_interned_string(buf, table)?,
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
            _unknown => skip_unknown_value(buf, table)?,
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
    let mut chunks = Vec::with_capacity(buf.capped_capacity(count as usize));
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
                if len != TRACE_ID_LEN {
                    return Err(DecodeError::InvalidFormat(format!(
                        "V1 chunk trace_id must be {TRACE_ID_LEN} bytes, got {len}"
                    )));
                }
                let bytes = buf
                    .try_slice_and_advance(TRACE_ID_LEN as usize)
                    .ok_or_else(|| {
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
                let mut spans = Vec::with_capacity(buf.capped_capacity(count as usize));
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
                chunk.priority = Some(i32::try_from(v).map_err(|_| {
                    DecodeError::InvalidFormat(format!("V1 chunk priority {v} exceeds i32 range"))
                })?);
            }
            chunk_key::SAMPLING_MECHANISM => {
                let v: u64 = decode::read_int(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat(
                        "V1 chunk sampling_mechanism read failure".to_owned(),
                    )
                })?;
                chunk.sampling_mechanism = Some(u32::try_from(v).map_err(|_| {
                    DecodeError::InvalidFormat(format!(
                        "V1 chunk sampling_mechanism {v} exceeds u32::MAX"
                    ))
                })?);
            }
            chunk_key::ATTRIBUTES => {
                // TODO: move promoted tags (`libdd_trace_model::PROMOTED_CHUNK_TAGS`) found in the
                // attributes to their dedicated fields, so decoded chunks uphold the model's
                // contract that promoted tags never live in `attributes`. A dedicated field set on
                // the wire should win over an attribute of the same name.
                chunk.attributes = span::read_attributes_map(buf, table)?;
            }
            chunk_key::DROPPED_TRACE => {
                chunk.dropped_trace = decode::read_bool(buf.as_mut_slice()).map_err(|_| {
                    DecodeError::InvalidFormat(
                        "V1 chunk dropped_trace bool read failure".to_owned(),
                    )
                })?;
            }
            _unknown => skip_unknown_value(buf, table)?,
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
    use crate::msgpack_encoder::v1::to_vec_from_v1;
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
            AttributeValue::Array(vec![AttributeValue::Int(1), AttributeValue::Int(2)]),
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
        let bytes = to_vec_from_v1(&original);
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
        let bytes = to_vec_from_v1(&original);
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
        let bytes = to_vec_from_v1(&payload);
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
        attrs.insert(bs("nested"), AttributeValue::KeyValueList(inner));

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
        let bytes = to_vec_from_v1(&payload);
        let (decoded, _) = from_bytes(Bytes::from(bytes)).expect("nested KeyValue roundtrip");

        let decoded_attrs = &decoded.chunks[0].spans[0].attributes;
        match decoded_attrs.get(&bs("nested")) {
            Some(AttributeValue::KeyValueList(map)) => {
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

                    let encoded = to_vec_from_v1(&payload);
                    let result = from_bytes(Bytes::from(encoded));
                    assert!(
                        result.is_ok(),
                        "decoder rejected its own encoded output: {result:?}"
                    );
                },
            );
    }

    // ---------------------------------------------------------------------------------------------
    // Forward-compatibility: unknown map keys must be skipped for every V1 map type. This test
    // hand-builds wire bytes (the encoder never emits unknown keys) with `rmp::encode`, injecting
    // a future/unknown key at every nesting level (payload, chunk, span, span_link, span_event),
    // and asserts the surrounding known fields still decode correctly.
    // ---------------------------------------------------------------------------------------------

    use rmp::encode::{self, ByteBuf};

    /// Writes a `u8` msgpack map key.
    fn wkey(buf: &mut ByteBuf, k: u8) {
        encode::write_uint(buf, k as u64).unwrap();
    }

    /// Exercises unknown-key skipping at every V1 nesting level in a single payload:
    /// - payload: unknown field 99 carries the first occurrence of "gh\xffost", invalid UTF-8 that
    ///   must still be harvested (repaired) as table id 1; a scalar skip at the chunk level
    ///   exercises the recursive skip, and a subsequent span field back-references "prod" by id to
    ///   prove the table wasn't desynced.
    /// - chunk: unknown field 77 carries a nested `[uint, str, map]` value (recursive skip), whose
    ///   inline string "buried" must also be harvested (table id 3).
    /// - span: unknown field 88 carries a scalar (f64) with no string to harvest.
    /// - span_link / span_event: unknown fields 55 / 66 precede their known sibling field.
    #[test]
    fn unknown_keys_are_skipped_at_every_level() {
        let mut span_link = ByteBuf::new();
        encode::write_map_len(&mut span_link, 2).unwrap();
        wkey(&mut span_link, 55); // unknown span_link key
        encode::write_bool(&mut span_link, false).unwrap();
        wkey(&mut span_link, span_link_key::SPAN_ID);
        encode::write_uint(&mut span_link, 777).unwrap();

        let mut span_event = ByteBuf::new();
        encode::write_map_len(&mut span_event, 2).unwrap();
        wkey(&mut span_event, 66); // unknown span_event key
        encode::write_uint(&mut span_event, 999).unwrap();
        wkey(&mut span_event, span_event_key::TIME);
        encode::write_uint(&mut span_event, 123).unwrap();

        let mut span = ByteBuf::new();
        encode::write_map_len(&mut span, 6).unwrap();
        wkey(&mut span, span_key::SPAN_ID);
        encode::write_uint(&mut span, 42).unwrap();
        wkey(&mut span, span_key::START);
        encode::write_uint(&mut span, 100).unwrap();
        wkey(&mut span, 88); // unknown span key: scalar, nothing to harvest
        encode::write_f64(&mut span, 2.5).unwrap();
        wkey(&mut span, span_key::SERVICE);
        encode::write_uint(&mut span, 2).unwrap(); // back-reference to encoder id 2 ("prod")
        wkey(&mut span, span_key::SPAN_LINKS);
        encode::write_array_len(&mut span, 1).unwrap();
        span.as_mut_vec().extend_from_slice(&span_link.into_vec());
        wkey(&mut span, span_key::SPAN_EVENTS);
        encode::write_array_len(&mut span, 1).unwrap();
        span.as_mut_vec().extend_from_slice(&span_event.into_vec());

        let mut chunk = ByteBuf::new();
        encode::write_map_len(&mut chunk, 3).unwrap();
        wkey(&mut chunk, 77); // unknown chunk key: nested value, recursive skip + string harvest
        encode::write_array_len(&mut chunk, 3).unwrap();
        encode::write_uint(&mut chunk, 1).unwrap();
        encode::write_str(&mut chunk, "buried").unwrap(); // first occurrence -> table id 3
        encode::write_map_len(&mut chunk, 1).unwrap();
        encode::write_uint(&mut chunk, 5).unwrap();
        encode::write_bool(&mut chunk, true).unwrap();
        wkey(&mut chunk, chunk_key::TRACE_ID);
        encode::write_bin(&mut chunk, &[9u8; 16]).unwrap();
        wkey(&mut chunk, chunk_key::SPANS);
        encode::write_array_len(&mut chunk, 1).unwrap();
        chunk.as_mut_vec().extend_from_slice(&span.into_vec());

        let mut buf = ByteBuf::new();
        encode::write_map_len(&mut buf, 3).unwrap();
        wkey(&mut buf, 99); // unknown payload key: first occurrence, invalid UTF-8 -> table id 1
        wstr_bytes(&mut buf, b"gh\xffost");
        wkey(&mut buf, trace_key::ENV_REF);
        encode::write_str(&mut buf, "prod").unwrap(); // first occurrence -> table id 2
        wkey(&mut buf, trace_key::CHUNKS);
        encode::write_array_len(&mut buf, 1).unwrap();
        buf.as_mut_vec().extend_from_slice(&chunk.into_vec());
        let buf = buf.into_vec();

        let (decoded, consumed) =
            from_bytes(Bytes::from(buf.clone())).expect("unknown keys must be skipped");
        assert_eq!(
            consumed,
            buf.len(),
            "decoder must consume every skipped value"
        );
        assert_eq!(decoded.env.as_str(), "prod");
        let chunk = &decoded.chunks[0];
        assert_eq!(chunk.trace_id, [9u8; 16]);
        let span = &chunk.spans[0];
        assert_eq!(span.span_id, 42);
        assert_eq!(span.start, 100);
        assert_eq!(
            span.service.as_str(),
            "prod",
            "back-reference must still resolve correctly: harvesting \"gh\\xffost\" and \
             \"buried\" while skipping unknown fields must not desync the string table"
        );
        assert_eq!(span.span_links[0].span_id, 777);
        assert_eq!(span.span_events[0].time_unix_nano, 123);
    }

    /// Writes `bytes` as a msgpack `str`, whether or not they are valid UTF-8.
    fn wstr_bytes(buf: &mut ByteBuf, bytes: &[u8]) {
        encode::write_str_len(buf, u32::try_from(bytes.len()).unwrap()).unwrap();
        buf.as_mut_vec().extend_from_slice(bytes);
    }

    /// A V1 streaming string on the wire: inline `str` bytes, or a `uint` table reference.
    enum Wire {
        Str(&'static [u8]),
        Ref(u64),
    }

    fn wstreaming(buf: &mut ByteBuf, value: &Wire) {
        match value {
            Wire::Str(bytes) => wstr_bytes(buf, bytes),
            Wire::Ref(id) => {
                encode::write_uint(buf, *id).unwrap();
            }
        }
    }

    /// Builds a payload whose optional unknown payload field 99 carries `harvested` strings
    /// (taking wire indices 1, 2, ... in order), followed by one chunk with one span per
    /// `(key, value)` pair, each span holding that single string attribute.
    fn payload_with_string_attrs(harvested: &[&[u8]], spans: &[(Wire, Wire)]) -> Vec<u8> {
        let mut buf = ByteBuf::new();
        if harvested.is_empty() {
            encode::write_map_len(&mut buf, 1).unwrap();
        } else {
            encode::write_map_len(&mut buf, 2).unwrap();
            wkey(&mut buf, 99);
            encode::write_array_len(&mut buf, u32::try_from(harvested.len()).unwrap()).unwrap();
            for s in harvested {
                wstr_bytes(&mut buf, s);
            }
        }
        wkey(&mut buf, trace_key::CHUNKS);
        encode::write_array_len(&mut buf, 1).unwrap();
        encode::write_map_len(&mut buf, 2).unwrap();
        wkey(&mut buf, chunk_key::TRACE_ID);
        encode::write_bin(&mut buf, &[1u8; 16]).unwrap();
        wkey(&mut buf, chunk_key::SPANS);
        encode::write_array_len(&mut buf, u32::try_from(spans.len()).unwrap()).unwrap();
        for (i, (key, value)) in spans.iter().enumerate() {
            encode::write_map_len(&mut buf, 3).unwrap();
            wkey(&mut buf, span_key::SPAN_ID);
            encode::write_uint(&mut buf, u64::try_from(i).unwrap() + 1).unwrap();
            wkey(&mut buf, span_key::START);
            encode::write_uint(&mut buf, 100).unwrap();
            wkey(&mut buf, span_key::ATTRIBUTES);
            encode::write_array_len(&mut buf, FLAT_ATTR_STRIDE).unwrap();
            wstreaming(&mut buf, key);
            encode::write_uint(&mut buf, u64::from(ANY_VALUE_KEY_STRING)).unwrap();
            wstreaming(&mut buf, value);
        }
        buf.into_vec()
    }

    /// For each span: its attribute count, and the string value of attribute `key` (if any).
    fn span_string_attrs<T: DeserializableTraceData>(
        payload: &TracerPayload<T>,
        key: &str,
    ) -> Vec<(usize, Option<String>)>
    where
        T::Text: Clone,
    {
        payload.chunks[0]
            .spans
            .iter()
            .map(|span| {
                let value = match span.attributes.get(key) {
                    Some(AttributeValue::String(s)) => Some(Borrow::<str>::borrow(s).to_owned()),
                    _ => None,
                };
                (span.attributes.len(), value)
            })
            .collect()
    }

    /// Inline strings with invalid UTF-8 are repaired the way the Agent repairs them (one U+FFFD
    /// per invalid byte), in both decoding modes, and a back-reference to their wire index
    /// resolves to the repaired value.
    #[test]
    fn invalid_utf8_inline_strings_are_repaired() {
        let cases: [(&'static [u8], &str); 8] = [
            (b"", ""),
            (b"elasticsearch.body", "elasticsearch.body"),
            (
                "café 日本語 🐶 \u{FFFD}".as_bytes(),
                "café 日本語 🐶 \u{FFFD}",
            ),
            (b"caf\xc3", "caf\u{FFFD}"),
            (b"body\xe2\x82", "body\u{FFFD}\u{FFFD}"),
            (b"body\xf0\x9f\x90", "body\u{FFFD}\u{FFFD}\u{FFFD}"),
            (b"\xe6\x97\xa5\xff\xfe\xe6\x9c\xac", "日\u{FFFD}\u{FFFD}本"),
            (b"bad\x80value", "bad\u{FFFD}value"),
        ];
        for (input, want) in cases {
            // Key inline (wire index 1), value back-references it.
            let bytes = payload_with_string_attrs(&[], &[(Wire::Str(input), Wire::Ref(1))]);
            let expected = vec![(1, Some(want.to_owned()))];

            let (owned, consumed) = from_bytes(Bytes::from(bytes.clone()))
                .unwrap_or_else(|e| panic!("{input:?} must be repaired, got {e}"));
            assert_eq!(consumed, bytes.len());
            assert_eq!(
                span_string_attrs(&owned, want),
                expected,
                "owned, {input:?}"
            );

            let (borrowed, _) = from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{input:?} must be repaired, got {e}"));
            assert_eq!(
                span_string_attrs(&borrowed, want),
                expected,
                "slice, {input:?}"
            );
        }
    }

    /// Modeled on the Agent's `TestUnmarshalTracerPayloadUTF8KeyCollision`: distinct wire
    /// strings that repair to the same value each keep their own wire index, and are one
    /// attribute key, so a lookup by the repaired name finds the attribute on every span.
    #[test]
    fn repaired_utf8_key_collision() {
        /// `(name, unknown-field strings, per-span (key, value), expected values)`.
        type Case = (
            &'static str,
            Vec<&'static [u8]>,
            Vec<(Wire, Wire)>,
            Vec<&'static str>,
        );
        const KEY: &str = "user\u{FFFD}";
        let cases: [Case; 2] = [
            (
                "inline keys",
                vec![],
                vec![
                    (Wire::Str(b"user\x80"), Wire::Str(b"alice")), // wire indices 1, 2
                    (Wire::Str(b"user\x81"), Wire::Str(b"bob")),   // wire indices 3, 4
                    (Wire::Ref(3), Wire::Str(b"carol")),           /* reference to the second
                                                                    * repaired key */
                ],
                vec!["alice", "bob", "carol"],
            ),
            (
                // libdatadog has no explicit string table field: strings taken from an unknown
                // field fill the table ahead of the spans that reference them by index.
                "string table keys",
                vec![b"user\x80", b"user\x81", b"alice", b"bob"], // wire indices 1..=4
                vec![(Wire::Ref(1), Wire::Ref(3)), (Wire::Ref(2), Wire::Ref(4))],
                vec!["alice", "bob"],
            ),
        ];
        for (name, harvested, spans, want) in cases {
            let bytes = payload_with_string_attrs(&harvested, &spans);
            let expected: Vec<_> = want.iter().map(|v| (1, Some((*v).to_owned()))).collect();

            let (owned, _) = from_bytes(Bytes::from(bytes.clone()))
                .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            assert_eq!(span_string_attrs(&owned, KEY), expected, "owned, {name}");
            let (borrowed, _) =
                from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
            assert_eq!(span_string_attrs(&borrowed, KEY), expected, "slice, {name}");

            // Updating by name must overwrite each span's attribute, not add a second one.
            let mut owned = owned;
            for span in &mut owned.chunks[0].spans {
                span.attributes
                    .insert_or_replace(bs(KEY), AttributeValue::String(bs("redacted")));
            }
            let redacted = vec![(1, Some("redacted".to_owned())); want.len()];
            assert_eq!(span_string_attrs(&owned, KEY), redacted, "{name}");
        }
    }

    /// Re-encoding a payload whose distinct wire strings repaired to the same value sends that
    /// value inline once: the encoder interns by value, so it never re-sends a duplicate.
    #[test]
    fn reencoding_repaired_collision_sends_each_string_once() {
        let bytes = payload_with_string_attrs(
            &[b"user\x82"], // unknown-field string that is never referenced
            &[
                (Wire::Str(b"user\x80"), Wire::Str(b"alice")),
                (Wire::Str(b"user\x81"), Wire::Str(b"bob")),
            ],
        );
        let (decoded, _) = from_bytes(Bytes::from(bytes)).expect("repaired payload decodes");
        let reencoded = to_vec_from_v1(&decoded);

        let repaired = "user\u{FFFD}".as_bytes();
        let inline_count = reencoded
            .windows(repaired.len())
            .filter(|w| *w == repaired)
            .count();
        assert_eq!(
            inline_count, 1,
            "repaired key must be sent inline exactly once"
        );

        let (roundtrip, _) = from_bytes(Bytes::from(reencoded)).expect("re-encoded payload");
        assert_eq!(
            span_string_attrs(&roundtrip, "user\u{FFFD}"),
            vec![(1, Some("alice".to_owned())), (1, Some("bob".to_owned()))]
        );
    }

    /// A payload with one span whose attributes are `[key, type, value]` triplets written by
    /// `write_value`.
    fn payload_with_double(write_value: impl FnOnce(&mut ByteBuf)) -> Vec<u8> {
        let mut buf = ByteBuf::new();
        encode::write_map_len(&mut buf, 1).unwrap();
        wkey(&mut buf, trace_key::CHUNKS);
        encode::write_array_len(&mut buf, 1).unwrap();
        encode::write_map_len(&mut buf, 2).unwrap();
        wkey(&mut buf, chunk_key::TRACE_ID);
        encode::write_bin(&mut buf, &[1u8; 16]).unwrap();
        wkey(&mut buf, chunk_key::SPANS);
        encode::write_array_len(&mut buf, 1).unwrap();
        encode::write_map_len(&mut buf, 3).unwrap();
        wkey(&mut buf, span_key::SPAN_ID);
        encode::write_uint(&mut buf, 1).unwrap();
        wkey(&mut buf, span_key::START);
        encode::write_uint(&mut buf, 100).unwrap();
        wkey(&mut buf, span_key::ATTRIBUTES);
        encode::write_array_len(&mut buf, 3).unwrap();
        encode::write_str(&mut buf, "ratio").unwrap();
        encode::write_uint(&mut buf, u64::from(ANY_VALUE_KEY_DOUBLE)).unwrap();
        write_value(&mut buf);
        buf.into_vec()
    }

    fn decoded_ratio(bytes: Vec<u8>) -> Result<f64, DecodeError> {
        let (payload, _) = from_bytes(Bytes::from(bytes))?;
        match payload.chunks[0].spans[0].attributes.get("ratio") {
            Some(AttributeValue::Float(f)) => Ok(*f),
            other => panic!("unexpected attribute {other:?}"),
        }
    }

    #[test]
    fn double_attribute_accepts_float32_and_float64() {
        // Tracers may write a double as float32 when that holds it exactly, as the agent reads.
        let f32_bytes = payload_with_double(|b| encode::write_f32(b, 1.5).unwrap());
        assert_eq!(decoded_ratio(f32_bytes).unwrap(), 1.5);
        let f64_bytes = payload_with_double(|b| encode::write_f64(b, 0.1).unwrap());
        assert_eq!(decoded_ratio(f64_bytes).unwrap(), 0.1);
    }

    #[test]
    fn double_attribute_rejects_integers() {
        let int_bytes = payload_with_double(|b| {
            encode::write_uint(b, 1).unwrap();
        });
        assert!(matches!(
            decoded_ratio(int_bytes),
            Err(DecodeError::InvalidFormat(m)) if m.contains("Double")
        ));
    }
}
