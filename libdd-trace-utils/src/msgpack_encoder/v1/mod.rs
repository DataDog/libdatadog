// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span_v04;
mod span_v1;

use crate::span::v04::Span;
use crate::span::v1::TracerPayload;
use crate::span::TraceData;
use crate::tracer_metadata::TracerMetadata;
use libdd_common::ResultInfallibleExt;
use rmp::encode::{
    write_array_len, write_bin, write_map_len, write_sint, write_str, write_uint, write_uint8,
    ByteBuf, RmpWrite, ValueWriteError,
};
use std::borrow::Borrow;
use std::collections::HashMap;

/// Integer keys for the top-level V1 trace payload map.
mod trace_key {
    pub const LANGUAGE_NAME: u8 = 3;
    pub const LANGUAGE_VERSION: u8 = 4;
    pub const TRACER_VERSION: u8 = 5;
    pub const RUNTIME_ID: u8 = 6;
    pub const ENV_REF: u8 = 7;
    pub const HOSTNAME_REF: u8 = 8;
    pub const APP_VERSION_REF: u8 = 9;
    /// Payload-level attributes map (e.g. `_dd.apm_mode`, `_dd.git.commit.sha`).
    pub const ATTRIBUTES: u8 = 10;
    pub const CHUNKS: u8 = 11;
}

/// Integer keys for V1 chunk-level fields.
mod chunk_key {
    pub const PRIORITY: u8 = 1;
    pub const ORIGIN: u8 = 2;
    pub const ATTRIBUTES: u8 = 3;
    pub const SPANS: u8 = 4;
    pub const DROPPED_TRACE: u8 = 5;
    pub const TRACE_ID: u8 = 6;
    /// Sampling mechanism (previously the `_dd.p.dm` span tag).
    pub const SAMPLING_MECHANISM: u8 = 7;
}

/// Streaming string intern table.
///
/// The first time a string is written, it is emitted as a msgpack `str` and assigned an
/// incrementing integer ID. On subsequent occurrences only the ID is emitted as a msgpack `uint`.
/// ID 0 is reserved for the empty string (pre-inserted in the constructor).
///
/// The string table is scoped per payload: each `to_vec` / `write_to_slice` call starts with a
/// fresh table so deduplication is payload-local.
pub(crate) struct StringTable {
    seen: HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        let mut seen = HashMap::new();
        seen.insert(String::new(), 0);
        Self { seen }
    }

    /// Writes `s` to `writer` using string interning.
    ///
    /// - First occurrence of `s` → msgpack `str`, ID recorded for future references
    /// - Subsequent occurrence → msgpack `uint` carrying the previously assigned ID
    pub(crate) fn write_interned<W: RmpWrite, S: AsRef<str>>(
        &mut self,
        writer: &mut W,
        s: S,
    ) -> Result<(), ValueWriteError<W::Error>> {
        let s = s.as_ref();
        if let Some(&id) = self.seen.get(s) {
            write_uint(writer, id as u64)?;
        } else {
            let id = self.seen.len() as u32;
            self.seen.insert(s.to_string(), id);
            write_str(writer, s)?;
        }
        Ok(())
    }
}

/// Promoted fields extracted from the payload's spans, written at the top-level map.
struct PayloadAttrs<'a> {
    env: Option<&'a str>,
    hostname: Option<&'a str>,
    app_version: Option<&'a str>,
    /// `_dd.apm_mode` span tag, promoted to payload-level attributes.
    apm_mode: Option<&'a str>,
    /// `_dd.git.commit.sha` span tag, promoted to payload-level attributes.
    git_commit_sha: Option<&'a str>,
}

fn extract_payload_attrs<'a, T: TraceData + 'a, S: AsRef<[Span<T>]>>(
    traces: &'a [S],
    metadata: &'a TracerMetadata,
) -> PayloadAttrs<'a>
where
    T::Text: 'a,
{
    // Prefer TracerMetadata (set once on the builder) over span scanning. Fall back to
    // span meta only when the builder-level value is missing — e.g. v04 payloads where
    // the SDK propagated these as span tags.
    let mut env = (!metadata.env.is_empty()).then_some(metadata.env.as_str());
    let mut hostname = (!metadata.hostname.is_empty()).then_some(metadata.hostname.as_str());
    let mut app_version =
        (!metadata.app_version.is_empty()).then_some(metadata.app_version.as_str());
    let mut git_commit_sha =
        (!metadata.git_commit_sha.is_empty()).then_some(metadata.git_commit_sha.as_str());
    let mut apm_mode = None;

    'outer: for trace in traces {
        for span in trace.as_ref() {
            if env.is_none() {
                env = span.meta.get("env").map(|v| v.borrow());
            }
            if hostname.is_none() {
                hostname = span.meta.get("_dd.hostname").map(|v| v.borrow());
            }
            if app_version.is_none() {
                app_version = span.meta.get("version").map(|v| v.borrow());
            }
            if apm_mode.is_none() {
                apm_mode = span.meta.get("_dd.apm_mode").map(|v| v.borrow());
            }
            if git_commit_sha.is_none() {
                git_commit_sha = span.meta.get("_dd.git.commit.sha").map(|v| v.borrow());
            }
            if env.is_some()
                && hostname.is_some()
                && app_version.is_some()
                && apm_mode.is_some()
                && git_commit_sha.is_some()
            {
                break 'outer;
            }
        }
    }

    PayloadAttrs {
        env,
        hostname,
        app_version,
        apm_mode,
        git_commit_sha,
    }
}

/// Promoted fields extracted from spans and written at the chunk level.
struct ChunkAttrs<'a> {
    /// Full 128-bit trace ID (encodes as 16-byte big-endian binary).
    trace_id: u128,
    /// Sampling priority from `_sampling_priority_v1` metric on the root span.
    sampling_priority: Option<i32>,
    /// Origin tag from `_dd.origin` meta on the root span.
    origin: Option<&'a str>,
    /// Sampling mechanism from `_dd.p.dm` meta on the root span.
    sampling_mechanism: Option<u32>,
}

fn extract_chunk_attrs<'a, T: TraceData>(spans: &'a [Span<T>]) -> ChunkAttrs<'a>
where
    T::Text: 'a,
{
    // trace_id is invariant per chunk. The v04 wire format carries only the low 64 bits;
    // the high 64 bits are propagated as the hex string meta tag "_dd.p.tid".
    let trace_id = spans
        .first()
        .map(|s| {
            let high = s
                .meta
                .get("_dd.p.tid")
                .and_then(|v| u64::from_str_radix(v.borrow(), 16).ok())
                .unwrap_or(0);
            ((high as u128) << 64) | s.trace_id
        })
        .unwrap_or(0);

    let mut sampling_priority = None;
    let mut origin = None;
    let mut sampling_mechanism = None;

    for span in spans {
        // Root span: either no parent in this chunk, or tagged _dd.top_level=1 (remote parent).
        let is_root =
            span.parent_id == 0 || span.metrics.get("_dd.top_level").copied().unwrap_or(0.0) == 1.0;

        if is_root {
            // Root span is authoritative: its values supersede any non-root fallback,
            // including absence (a field missing on the root should not be filled from non-roots).
            sampling_priority = span.metrics.get("_sampling_priority_v1").map(|v| *v as i32);
            origin = span.meta.get("_dd.origin").map(|v| v.borrow());
            // _dd.p.dm is a signed integer stored as a string; unsigned_abs preserves the
            // magnitude.
            sampling_mechanism = span
                .meta
                .get("_dd.p.dm")
                .and_then(|v| v.borrow().parse::<i32>().ok())
                .map(|dm| dm.unsigned_abs());
            break;
        }

        // No root found yet — accumulate fallback values from non-root spans (partial flush).
        // Root span values will override these if a root is eventually encountered.
        if sampling_priority.is_none() {
            sampling_priority = span.metrics.get("_sampling_priority_v1").map(|v| *v as i32);
        }
        if origin.is_none() {
            origin = span.meta.get("_dd.origin").map(|v| v.borrow());
        }
        if sampling_mechanism.is_none() {
            sampling_mechanism = span
                .meta
                .get("_dd.p.dm")
                .and_then(|v| v.borrow().parse::<i32>().ok())
                .map(|dm| dm.unsigned_abs());
        }
    }

    ChunkAttrs {
        trace_id,
        sampling_priority,
        origin,
        sampling_mechanism,
    }
}

/// Encodes all traces as a V1 msgpack payload.
///
/// Top-level format:
/// ```text
/// Map {
///   trace_key::ENV_REF      (7)  → str|uint       // optional, interned
///   trace_key::HOSTNAME_REF (8)  → str|uint       // optional, interned
///   trace_key::APP_VERSION  (9)  → str|uint       // optional, interned
///   trace_key::ATTRIBUTES   (10) → Array[...]     // optional, flat triplets: key, type, value
///   trace_key::CHUNKS       (11) → Array[Chunk, ...]
/// }
/// ```
fn encode_payload<W: RmpWrite, T: TraceData, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    traces: &[S],
    metadata: &TracerMetadata,
) -> Result<(), ValueWriteError<W::Error>> {
    let mut table = StringTable::new();
    let payload_attrs = extract_payload_attrs(traces, metadata);

    let attr_count =
        payload_attrs.apm_mode.is_some() as u32 + payload_attrs.git_commit_sha.is_some() as u32;
    let has_attributes = attr_count > 0;

    let map_len = 1u32 // chunks always present
        + (!metadata.language.is_empty()) as u32
        + (!metadata.language_version.is_empty()) as u32
        + (!metadata.tracer_version.is_empty()) as u32
        + (!metadata.runtime_id.is_empty()) as u32
        + payload_attrs.env.is_some() as u32
        + payload_attrs.hostname.is_some() as u32
        + payload_attrs.app_version.is_some() as u32
        + has_attributes as u32;

    write_map_len(writer, map_len)?;

    if !metadata.language.is_empty() {
        write_uint8(writer, trace_key::LANGUAGE_NAME)?;
        table.write_interned(writer, &metadata.language)?;
    }

    if !metadata.language_version.is_empty() {
        write_uint8(writer, trace_key::LANGUAGE_VERSION)?;
        table.write_interned(writer, &metadata.language_version)?;
    }

    if !metadata.tracer_version.is_empty() {
        write_uint8(writer, trace_key::TRACER_VERSION)?;
        table.write_interned(writer, &metadata.tracer_version)?;
    }

    if !metadata.runtime_id.is_empty() {
        write_uint8(writer, trace_key::RUNTIME_ID)?;
        table.write_interned(writer, &metadata.runtime_id)?;
    }

    if let Some(env) = payload_attrs.env {
        write_uint8(writer, trace_key::ENV_REF)?;
        table.write_interned(writer, env)?;
    }

    if let Some(hostname) = payload_attrs.hostname {
        write_uint8(writer, trace_key::HOSTNAME_REF)?;
        table.write_interned(writer, hostname)?;
    }

    if let Some(app_version) = payload_attrs.app_version {
        write_uint8(writer, trace_key::APP_VERSION_REF)?;
        table.write_interned(writer, app_version)?;
    }

    if has_attributes {
        // Encoded as a flat array of triplets: [key, type_uint, value, ...]
        // String values use type discriminant 1.
        write_uint8(writer, trace_key::ATTRIBUTES)?;
        write_array_len(writer, attr_count * 3)?;
        if let Some(v) = payload_attrs.apm_mode {
            table.write_interned(writer, "_dd.apm_mode")?;
            write_uint8(writer, span_v04::AnyValueKey::String as u8)?;
            table.write_interned(writer, v)?;
        }
        if let Some(v) = payload_attrs.git_commit_sha {
            table.write_interned(writer, "_dd.git.commit.sha")?;
            write_uint8(writer, span_v04::AnyValueKey::String as u8)?;
            table.write_interned(writer, v)?;
        }
    }

    write_uint8(writer, trace_key::CHUNKS)?;
    write_array_len(writer, traces.len() as u32)?;
    for trace in traces {
        encode_chunk(writer, trace.as_ref(), &mut table)?;
    }

    Ok(())
}

/// Encodes one chunk (a group of spans sharing a trace ID).
///
/// ```text
/// Map {
///   chunk_key::TRACE_ID           (6) → bin[16]       // 128-bit big-endian
///   chunk_key::ORIGIN             (2) → str|uint       // optional, interned
///   chunk_key::PRIORITY           (1) → int            // optional
///   chunk_key::SAMPLING_MECHANISM (7) → uint           // optional
///   chunk_key::SPANS              (4) → Array[Span, ...]
/// }
/// ```
fn encode_chunk<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    spans: &[Span<T>],
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    let attrs = extract_chunk_attrs(spans);

    let fields = 2u32 // trace_id + spans are always present
        + attrs.origin.is_some() as u32
        + attrs.sampling_priority.is_some() as u32
        + attrs.sampling_mechanism.is_some() as u32;

    write_map_len(writer, fields)?;

    write_uint8(writer, chunk_key::TRACE_ID)?;
    write_bin(writer, &attrs.trace_id.to_be_bytes())?;

    if let Some(origin) = attrs.origin {
        write_uint8(writer, chunk_key::ORIGIN)?;
        table.write_interned(writer, origin)?;
    }

    if let Some(priority) = attrs.sampling_priority {
        write_uint8(writer, chunk_key::PRIORITY)?;
        write_sint(writer, priority as i64)?;
    }

    if let Some(mechanism) = attrs.sampling_mechanism {
        write_uint8(writer, chunk_key::SAMPLING_MECHANISM)?;
        write_uint(writer, mechanism as u64)?;
    }

    write_uint8(writer, chunk_key::SPANS)?;
    write_array_len(writer, spans.len() as u32)?;
    for span in spans {
        span_v04::encode_span(writer, span, table)?;
    }

    Ok(())
}

/// Serializes traces into a slice using the V1 msgpack format.
///
/// # Errors
/// Returns a `ValueWriteError` if the underlying writer fails.
pub fn write_to_slice<T: TraceData, S: AsRef<[Span<T>]>>(
    // &mut &mut [u8] lets the caller see the slice shrink as bytes are written.
    slice: &mut &mut [u8],
    traces: &[S],
    metadata: &TracerMetadata,
) -> Result<(), ValueWriteError> {
    encode_payload(slice, traces, metadata)
}

/// Serializes traces into a `Vec<u8>` using the V1 msgpack format.
pub fn to_vec<T: TraceData, S: AsRef<[Span<T>]>>(
    traces: &[S],
    metadata: &TracerMetadata,
) -> Vec<u8> {
    to_vec_with_capacity(traces, 0, metadata)
}

/// Serializes traces into a `Vec<u8>` with a pre-allocated capacity.
pub fn to_vec_with_capacity<T: TraceData, S: AsRef<[Span<T>]>>(
    traces: &[S],
    capacity: u32,
    metadata: &TracerMetadata,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    encode_payload(&mut buf, traces, metadata)
        .map_err(super::flatten_value_write_infallible)
        .unwrap_infallible();
    buf.into_vec()
}

/// Returns the number of bytes the V1 payload for `traces` would occupy.
pub fn to_encoded_byte_len<T: TraceData, S: AsRef<[Span<T>]>>(
    traces: &[S],
    metadata: &TracerMetadata,
) -> u32 {
    let mut counter = super::CountLength(0);
    // `CountLength` impls `std::io::Write` (whose error type is `std::io::Error`, not
    // `Infallible`), so we can't statically prove infallibility via `unwrap_infallible`
    // the way we do for `ByteBuf`. In practice `CountLength::write*` only ever return
    // `Ok`, so the error path here is unreachable today; should `CountLength` ever grow
    // a fallible code path, fuzz tests on the msgpack encoded length would catch it.
    let _ = encode_payload(&mut counter, traces, metadata);
    counter.0
}

/// Encodes a [`TracerPayload`] (V1 data model) as a V1 msgpack payload.
fn encode_payload_v1<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    payload: &TracerPayload<T>,
) -> Result<(), ValueWriteError<W::Error>> {
    let mut table = StringTable::new();

    let has_attributes = !payload.attributes.is_empty();

    let map_len = 1u32 // chunks always present
        + (!payload.language_name.borrow().is_empty()) as u32
        + (!payload.language_version.borrow().is_empty()) as u32
        + (!payload.tracer_version.borrow().is_empty()) as u32
        + (!payload.runtime_id.borrow().is_empty()) as u32
        + (!payload.env.borrow().is_empty()) as u32
        + (!payload.hostname.borrow().is_empty()) as u32
        + (!payload.app_version.borrow().is_empty()) as u32
        + has_attributes as u32;

    write_map_len(writer, map_len)?;

    if !payload.language_name.borrow().is_empty() {
        write_uint8(writer, trace_key::LANGUAGE_NAME)?;
        table.write_interned(writer, payload.language_name.borrow())?;
    }

    if !payload.language_version.borrow().is_empty() {
        write_uint8(writer, trace_key::LANGUAGE_VERSION)?;
        table.write_interned(writer, payload.language_version.borrow())?;
    }

    if !payload.tracer_version.borrow().is_empty() {
        write_uint8(writer, trace_key::TRACER_VERSION)?;
        table.write_interned(writer, payload.tracer_version.borrow())?;
    }

    if !payload.runtime_id.borrow().is_empty() {
        write_uint8(writer, trace_key::RUNTIME_ID)?;
        table.write_interned(writer, payload.runtime_id.borrow())?;
    }

    if !payload.env.borrow().is_empty() {
        write_uint8(writer, trace_key::ENV_REF)?;
        table.write_interned(writer, payload.env.borrow())?;
    }

    if !payload.hostname.borrow().is_empty() {
        write_uint8(writer, trace_key::HOSTNAME_REF)?;
        table.write_interned(writer, payload.hostname.borrow())?;
    }

    if !payload.app_version.borrow().is_empty() {
        write_uint8(writer, trace_key::APP_VERSION_REF)?;
        table.write_interned(writer, payload.app_version.borrow())?;
    }

    if has_attributes {
        write_uint8(writer, trace_key::ATTRIBUTES)?;
        span_v1::encode_attributes_map(writer, &payload.attributes, &mut table)?;
    }

    write_uint8(writer, trace_key::CHUNKS)?;
    write_array_len(writer, payload.chunks.len() as u32)?;
    for chunk in &payload.chunks {
        encode_chunk_v1(writer, chunk, &mut table)?;
    }

    Ok(())
}

/// Encodes one V1 chunk (a group of spans sharing a trace ID).
fn encode_chunk_v1<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    chunk: &crate::span::v1::TraceChunk<T>,
    table: &mut StringTable,
) -> Result<(), ValueWriteError<W::Error>> {
    let has_origin = chunk
        .origin
        .as_ref()
        .is_some_and(|o| !<T::Text as Borrow<str>>::borrow(o).is_empty());
    let has_attributes = !chunk.attributes.is_empty();
    let has_dropped = chunk.dropped_trace;

    let fields = 2u32 // trace_id + spans
        + has_origin as u32
        + chunk.priority.is_some() as u32
        + chunk.sampling_mechanism.is_some() as u32
        + has_attributes as u32
        + has_dropped as u32;

    write_map_len(writer, fields)?;

    write_uint8(writer, chunk_key::TRACE_ID)?;
    write_bin(writer, &chunk.trace_id)?;

    if let Some(origin) = chunk
        .origin
        .as_ref()
        .filter(|o| !<T::Text as Borrow<str>>::borrow(o).is_empty())
    {
        write_uint8(writer, chunk_key::ORIGIN)?;
        table.write_interned(writer, <T::Text as Borrow<str>>::borrow(origin))?;
    }

    if let Some(priority) = chunk.priority {
        write_uint8(writer, chunk_key::PRIORITY)?;
        write_sint(writer, priority as i64)?;
    }

    if let Some(mechanism) = chunk.sampling_mechanism {
        write_uint8(writer, chunk_key::SAMPLING_MECHANISM)?;
        write_uint(writer, mechanism as u64)?;
    }

    if has_attributes {
        write_uint8(writer, chunk_key::ATTRIBUTES)?;
        span_v1::encode_attributes_map(writer, &chunk.attributes, table)?;
    }

    if has_dropped {
        write_uint8(writer, chunk_key::DROPPED_TRACE)?;
        rmp::encode::write_bool(writer, true).map_err(ValueWriteError::InvalidDataWrite)?;
    }

    write_uint8(writer, chunk_key::SPANS)?;
    write_array_len(writer, chunk.spans.len() as u32)?;
    for span in &chunk.spans {
        span_v1::encode_span(writer, span, table)?;
    }

    Ok(())
}

/// Serializes a `TracerPayload` into a vector of bytes with a default capacity of 0.
///
/// # Arguments
///
/// * `payload` - A reference to a `TracerPayload`.
///
/// # Returns
///
/// * `Vec<u8>` - A vector containing the encoded payload.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_payload;
/// use libdd_trace_utils::span::v1::TracerPayloadSlice;
///
/// let payload = TracerPayloadSlice {
///     language_name: "rust".into(),
///     ..Default::default()
/// };
/// let encoded = to_vec_from_payload(&payload);
///
/// assert!(!encoded.is_empty());
/// ```
pub fn to_vec_from_payload<T: TraceData>(payload: &TracerPayload<T>) -> Vec<u8> {
    to_vec_from_payload_with_capacity(payload, 0)
}

/// Serializes a `TracerPayload` into a vector of bytes with specified capacity.
///
/// # Arguments
///
/// * `payload` - A reference to a `TracerPayload`.
/// * `capacity` - Desired initial capacity of the resulting vector.
///
/// # Returns
///
/// * `Vec<u8>` - A vector containing the encoded payload.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_payload_with_capacity;
/// use libdd_trace_utils::span::v1::TracerPayloadSlice;
///
/// let payload = TracerPayloadSlice {
///     language_name: "rust".into(),
///     ..Default::default()
/// };
/// let encoded = to_vec_from_payload_with_capacity(&payload, 1024);
///
/// assert!(encoded.capacity() >= 1024);
/// ```
pub fn to_vec_from_payload_with_capacity<T: TraceData>(
    payload: &TracerPayload<T>,
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    encode_payload_v1(&mut buf, payload)
        .map_err(super::flatten_value_write_infallible)
        .unwrap_infallible();
    buf.into_vec()
}

/// Encodes a `TracerPayload` into a slice of bytes.
///
/// # Arguments
///
/// * `slice` - A mutable reference to a byte slice.
/// * `payload` - A reference to a `TracerPayload`.
///
/// # Returns
///
/// * `Ok(())` - If encoding succeeds.
/// * `Err(ValueWriteError)` - If encoding fails.
///
/// # Errors
///
/// This function will return an error if the underlying writer fails (e.g. buffer too small).
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v1::write_payload_to_slice;
/// use libdd_trace_utils::span::v1::TracerPayloadSlice;
///
/// let mut buffer = vec![0u8; 1024];
/// let payload = TracerPayloadSlice {
///     language_name: "rust".into(),
///     ..Default::default()
/// };
///
/// write_payload_to_slice(&mut &mut buffer[..], &payload).expect("Encoding failed");
/// ```
pub fn write_payload_to_slice<T: TraceData>(
    slice: &mut &mut [u8],
    payload: &TracerPayload<T>,
) -> Result<(), ValueWriteError> {
    encode_payload_v1(slice, payload)
}

/// Computes the number of bytes required to encode the given `TracerPayload`.
///
/// This does not allocate any actual buffer, but simulates writing in order to measure
/// the encoded size of the payload.
///
/// # Arguments
///
/// * `payload` - A reference to a `TracerPayload`.
///
/// # Returns
///
/// * `u32` - The number of bytes that would be written by the encoder.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v1::to_encoded_byte_len_from_payload;
/// use libdd_trace_utils::span::v1::TracerPayloadSlice;
///
/// let payload = TracerPayloadSlice {
///     language_name: "rust".into(),
///     ..Default::default()
/// };
/// let encoded_len = to_encoded_byte_len_from_payload(&payload);
///
/// assert!(encoded_len > 0);
/// ```
pub fn to_encoded_byte_len_from_payload<T: TraceData>(payload: &TracerPayload<T>) -> u32 {
    let mut counter = super::CountLength(0);
    let _ = encode_payload_v1(&mut counter, payload);
    counter.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::SpanBytes;
    use libdd_tinybytes::BytesString;
    use std::collections::HashMap;

    fn make_span(
        service: &str,
        name: &str,
        trace_id: u128,
        span_id: u64,
        parent_id: u64,
    ) -> SpanBytes {
        SpanBytes {
            service: BytesString::from_slice(service.as_bytes()).unwrap(),
            name: BytesString::from_slice(name.as_bytes()).unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id,
            span_id,
            parent_id,
            start: 1_000_000,
            duration: 500,
            ..Default::default()
        }
    }

    #[test]
    fn test_to_vec_non_empty() {
        let spans = vec![make_span("svc", "op", 42, 1, 0)];
        let traces = vec![spans];
        let encoded = to_vec(&traces, &TracerMetadata::default());
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_to_vec_empty_traces() {
        let traces: Vec<Vec<SpanBytes>> = vec![];
        let encoded = to_vec(&traces, &TracerMetadata::default());
        // Must still produce a valid msgpack map with an empty chunks array.
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_string_interning_reduces_size() {
        // Two spans with the same service name — second occurrence should use the integer ID.
        let s1 = make_span("my-service", "op1", 1, 1, 0);
        let s2 = make_span("my-service", "op2", 2, 2, 0);
        let traces_two = vec![vec![s1], vec![s2]];

        // Single span for baseline.
        let s_single = make_span("my-service", "op1", 1, 1, 0);
        let traces_single = vec![vec![s_single]];

        let encoded_two = to_vec(&traces_two, &TracerMetadata::default());
        let encoded_single = to_vec(&traces_single, &TracerMetadata::default());

        // The two-trace payload should be less than 2× the single-trace payload
        // if interning is working (the second "my-service" is encoded as an integer).
        assert!(
            encoded_two.len() < 2 * encoded_single.len(),
            "Interning should reduce size: two={} single={}",
            encoded_two.len(),
            encoded_single.len()
        );
    }

    #[test]
    fn test_chunk_level_attrs_origin_and_priority() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("_dd.origin"),
            BytesString::from_static("lambda"),
        );
        let mut metrics = HashMap::new();
        metrics.insert(BytesString::from_static("_sampling_priority_v1"), 1.0f64);

        let root = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 99,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            metrics,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![root]], &TracerMetadata::default());
        assert!(!encoded.is_empty());
        // The payload must contain "lambda" somewhere (the origin string).
        let lambda_bytes = b"lambda";
        assert!(
            encoded
                .windows(lambda_bytes.len())
                .any(|w| w == lambda_bytes),
            "origin 'lambda' should appear in payload"
        );
    }

    #[test]
    fn test_to_encoded_byte_len_matches_to_vec() {
        let spans = vec![
            make_span("svc", "op", 1, 1, 0),
            make_span("svc", "child", 1, 2, 1),
        ];
        let traces = vec![spans];
        let meta = TracerMetadata::default();
        let encoded = to_vec(&traces, &meta);
        let len = to_encoded_byte_len(&traces, &meta);
        assert_eq!(encoded.len() as u32, len);
    }

    #[test]
    fn test_remote_parent_root_span_top_level() {
        // A span with a non-zero parent_id but _dd.top_level=1.0 is a root in its chunk.
        let mut metrics = HashMap::new();
        metrics.insert(BytesString::from_static("_dd.top_level"), 1.0f64);
        metrics.insert(BytesString::from_static("_sampling_priority_v1"), 2.0f64);

        let root = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 123,
            span_id: 42,
            parent_id: 999, // remote parent — not in this chunk
            start: 1000,
            duration: 100,
            metrics,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![root]], &TracerMetadata::default());
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_payload_promoted_fields() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("env"),
            BytesString::from_static("prod"),
        );
        meta.insert(
            BytesString::from_static("version"),
            BytesString::from_static("1.2.3"),
        );
        meta.insert(
            BytesString::from_static("_dd.hostname"),
            BytesString::from_static("my-host"),
        );

        let span = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![span]], &TracerMetadata::default());
        let prod_bytes = b"prod";
        assert!(
            encoded.windows(prod_bytes.len()).any(|w| w == prod_bytes),
            "env 'prod' should appear in payload"
        );
        let host_bytes = b"my-host";
        assert!(
            encoded.windows(host_bytes.len()).any(|w| w == host_bytes),
            "hostname 'my-host' should appear in payload"
        );
    }

    #[test]
    fn test_payload_attributes_apm_mode_and_git_commit_sha() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("_dd.apm_mode"),
            BytesString::from_static("ssi"),
        );
        meta.insert(
            BytesString::from_static("_dd.git.commit.sha"),
            BytesString::from_static("abc123"),
        );

        let span = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        };

        let encoded = to_vec(&[vec![span]], &TracerMetadata::default());

        // Both attribute strings must appear in the payload bytes.
        let ssi_bytes = b"ssi";
        assert!(
            encoded.windows(ssi_bytes.len()).any(|w| w == ssi_bytes),
            "apm_mode 'ssi' should appear in payload"
        );
        let sha_bytes = b"abc123";
        assert!(
            encoded.windows(sha_bytes.len()).any(|w| w == sha_bytes),
            "git commit sha 'abc123' should appear in payload"
        );
        // The attribute key names must also be present (first occurrence is a raw str).
        let apm_key = b"_dd.apm_mode";
        assert!(
            encoded.windows(apm_key.len()).any(|w| w == apm_key),
            "_dd.apm_mode key should appear in payload"
        );
        let git_key = b"_dd.git.commit.sha";
        assert!(
            encoded.windows(git_key.len()).any(|w| w == git_key),
            "_dd.git.commit.sha key should appear in payload"
        );
    }

    #[test]
    fn test_payload_attributes_absent_when_no_relevant_tags() {
        // A span with no _dd.apm_mode or _dd.git.commit.sha must not produce key 10.
        let span = make_span("svc", "op", 1, 1, 0);
        let encoded = to_vec(&[vec![span]], &TracerMetadata::default());
        let apm_key = b"_dd.apm_mode";
        assert!(
            !encoded.windows(apm_key.len()).any(|w| w == apm_key),
            "key 10 should be absent when no relevant tags are set"
        );
    }

    #[test]
    fn test_payload_metadata_fields_present() {
        let span = make_span("svc", "op", 1, 1, 0);
        let metadata = TracerMetadata {
            language: "python".to_string(),
            language_version: "3.11".to_string(),
            tracer_version: "2.0.0".to_string(),
            runtime_id: "abc-123-uuid".to_string(),
            ..Default::default()
        };
        let encoded = to_vec(&[vec![span]], &metadata);

        for s in &[b"python" as &[u8], b"3.11", b"2.0.0", b"abc-123-uuid"] {
            assert!(
                encoded.windows(s.len()).any(|w| w == *s),
                "{} should appear in payload",
                std::str::from_utf8(s).unwrap()
            );
        }
    }

    #[test]
    fn test_payload_metadata_absent_when_empty() {
        let span = make_span("svc", "op", 1, 1, 0);
        let encoded_with = to_vec(
            &[vec![span.clone()]],
            &TracerMetadata {
                language: "go".to_string(),
                ..Default::default()
            },
        );
        let encoded_without = to_vec(&[vec![span]], &TracerMetadata::default());
        // Payload with metadata must be larger (it carries extra fields).
        assert!(encoded_with.len() > encoded_without.len());
    }

    #[test]
    fn test_128bit_trace_id_from_dd_p_tid() {
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("_dd.p.tid"),
            BytesString::from_static("640cfd5400000000"),
        );
        let span = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 0x0123456789abcdef,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        };
        let encoded = to_vec(&[vec![span]], &TracerMetadata::default());

        // Expected 16-byte BE: high = 0x640cfd5400000000, low = 0x0123456789abcdef
        let expected = [
            0x64, 0x0c, 0xfd, 0x54, 0x00, 0x00, 0x00, 0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert!(
            encoded.windows(16).any(|w| w == expected),
            "128-bit trace_id big-endian bytes should appear in payload"
        );
        // _dd.p.tid must not also leak into span attributes.
        let tid_key = b"_dd.p.tid";
        assert!(
            !encoded.windows(tid_key.len()).any(|w| w == tid_key),
            "_dd.p.tid should be consumed, not encoded as a span attribute"
        );
    }

    #[test]
    fn test_128bit_trace_id_without_dd_p_tid() {
        // Absent _dd.p.tid → high 64 bits zero.
        let span = make_span("svc", "op", 0x0123456789abcdef, 1, 0);
        let encoded = to_vec(&[vec![span]], &TracerMetadata::default());
        let expected = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert!(
            encoded.windows(16).any(|w| w == expected),
            "absent _dd.p.tid should yield zero high 64 bits"
        );
    }

    #[test]
    fn test_sampling_mechanism_negative_value() {
        // `_dd.p.dm` is a signed integer stored as a string (e.g. "-4" → manual rule).
        // The encoder must parse it, take unsigned_abs, and emit it at chunk level.
        let mut meta = HashMap::new();
        meta.insert(
            BytesString::from_static("_dd.p.dm"),
            BytesString::from_static("-4"),
        );
        let root = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        };
        let encoded = to_vec(&[vec![root]], &TracerMetadata::default());

        // The chunk-level sampling_mechanism (key 7) must be encoded as uint 4.
        // The byte sequence is `chunk_key::SAMPLING_MECHANISM (0x07)` followed by the
        // msgpack representation of 4 (positive fixint 0x04).
        let expected = [chunk_key::SAMPLING_MECHANISM, 0x04];
        assert!(
            encoded.windows(2).any(|w| w == expected),
            "sampling_mechanism should be encoded as unsigned_abs(\"-4\") = 4"
        );
    }

    #[test]
    fn test_chunk_attrs_fallback_no_root_span() {
        // Partial flush: no root span (every span has a non-zero parent and no
        // `_dd.top_level`). Values must be accumulated from non-root spans.
        let mut meta1 = HashMap::new();
        meta1.insert(
            BytesString::from_static("_dd.origin"),
            BytesString::from_static("lambda"),
        );
        let mut metrics2 = HashMap::new();
        metrics2.insert(BytesString::from_static("_sampling_priority_v1"), 2.0f64);
        let mut meta3 = HashMap::new();
        meta3.insert(
            BytesString::from_static("_dd.p.dm"),
            BytesString::from_static("-3"),
        );

        let s1 = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op1").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 11,
            parent_id: 10, // non-zero parent → not a root
            start: 1000,
            duration: 100,
            meta: meta1,
            ..Default::default()
        };
        let s2 = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op2").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 12,
            parent_id: 11,
            start: 1000,
            duration: 100,
            metrics: metrics2,
            ..Default::default()
        };
        let s3 = SpanBytes {
            service: BytesString::from_slice(b"svc").unwrap(),
            name: BytesString::from_slice(b"op3").unwrap(),
            resource: BytesString::from_slice(b"res").unwrap(),
            trace_id: 1,
            span_id: 13,
            parent_id: 12,
            start: 1000,
            duration: 100,
            meta: meta3,
            ..Default::default()
        };
        let encoded = to_vec(&[vec![s1, s2, s3]], &TracerMetadata::default());

        // Each attribute must be present at chunk level — collected from a different
        // non-root span.
        let lambda = b"lambda";
        assert!(
            encoded.windows(lambda.len()).any(|w| w == lambda),
            "origin 'lambda' from span 1 should appear in payload"
        );
        // priority 2 → msgpack positive fixint 0x02 preceded by PRIORITY key
        let prio = [chunk_key::PRIORITY, 0x02];
        assert!(
            encoded.windows(2).any(|w| w == prio),
            "sampling_priority 2 from span 2 should appear"
        );
        // sampling_mechanism = unsigned_abs("-3") = 3 → 0x03 preceded by SAMPLING_MECHANISM key
        let mech = [chunk_key::SAMPLING_MECHANISM, 0x03];
        assert!(
            encoded.windows(2).any(|w| w == mech),
            "sampling_mechanism 3 from span 3 should appear"
        );
    }
}

#[cfg(test)]
mod v1_payload_tests {
    //! Unit tests for the M3 encoder (`encode_payload_v1`).
    //!
    //! Verifies the encoder produces a valid V1 payload from the canonical
    //! [`crate::span::v1::TracerPayload`] data model and that core invariants (interning, byte
    //! length, optional fields) hold.

    use super::*;
    use crate::span::v1::{
        AttributeValue, Span as V1Span, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes,
        TracerPayloadBytes,
    };
    use libdd_tinybytes::BytesString;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap_or_default()
    }

    fn make_span(service: &str, name: &str, span_id: u64) -> V1SpanBytes {
        V1Span {
            service: bs(service),
            name: bs(name),
            resource: bs("res"),
            span_id,
            start: 1_000_000,
            duration: 500,
            ..Default::default()
        }
    }

    fn make_chunk(spans: Vec<V1SpanBytes>, trace_id: [u8; 16]) -> TraceChunkBytes {
        TraceChunkBytes {
            trace_id,
            spans,
            ..Default::default()
        }
    }

    #[test]
    fn empty_payload_is_valid_msgpack_map() {
        let payload = TracerPayloadBytes::default();
        let encoded = to_vec_from_payload(&payload);
        // Map with a single entry (chunks), then an empty array. `0x81` = fixmap of length 1,
        // followed by chunk key (0x0b), then `0x90` (fixarray length 0).
        assert_eq!(encoded, vec![0x81, 0x0b, 0x90]);
    }

    #[test]
    fn payload_byte_len_matches_to_vec() {
        let chunk = make_chunk(vec![make_span("svc", "op", 1)], [0u8; 16]);
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        let len = to_encoded_byte_len_from_payload(&payload);
        assert_eq!(encoded.len() as u32, len);
    }

    #[test]
    fn span_kind_is_always_emitted_as_uint() {
        // Default SpanKind (Internal=1) must be emitted. The encoded payload contains
        // `kind_key (0x10) | uint 1 (0x01)`.
        let chunk = make_chunk(vec![make_span("svc", "op", 1)], [0u8; 16]);
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        let pat = [0x10u8, 0x01u8];
        assert!(
            encoded.windows(2).any(|w| w == pat),
            "Kind (key=16) Internal (=1) must be emitted"
        );
    }

    #[test]
    fn typed_attributes_carry_correct_type_discriminants() {
        let mut attrs = HashMap::new();
        attrs.insert(bs("k_str"), AttributeValue::String(bs("v")));
        let span = V1Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            span_id: 1,
            start: 1,
            duration: 1,
            attributes: attrs,
            ..Default::default()
        };
        let chunk = make_chunk(vec![span], [0u8; 16]);
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        // String attribute → type discriminant = 1 (`AnyValueKey::String`).
        assert!(
            encoded.windows(b"k_str".len()).any(|w| w == b"k_str"),
            "attribute key must appear"
        );
    }

    #[test]
    fn bytes_attribute_uses_bin_marker() {
        // A Bytes attribute must use the msgpack `bin` family, not `str`.
        let mut attrs = HashMap::new();
        attrs.insert(
            bs("payload"),
            AttributeValue::Bytes(libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad")),
        );
        let span = V1Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            span_id: 1,
            start: 1,
            duration: 1,
            attributes: attrs,
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![make_chunk(vec![span], [0u8; 16])],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        // bin8 marker `0xc4` followed by length `0x02` and the bytes themselves.
        let want = [0xc4u8, 0x02, 0xde, 0xad];
        assert!(
            encoded.windows(4).any(|w| w == want),
            "Bytes attribute must be encoded as msgpack bin"
        );
    }

    #[test]
    fn list_and_keyvalue_attributes_round_trip_through_recursion() {
        let mut nested = HashMap::new();
        nested.insert(bs("nk"), AttributeValue::Int(7));
        let mut attrs = HashMap::new();
        attrs.insert(
            bs("list"),
            AttributeValue::List(vec![
                AttributeValue::String(bs("a")),
                AttributeValue::Bool(true),
            ]),
        );
        attrs.insert(bs("kv"), AttributeValue::KeyValue(nested));
        let span = V1Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            span_id: 1,
            start: 1,
            duration: 1,
            attributes: attrs,
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![make_chunk(vec![span], [0u8; 16])],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        // The keys and the nested key must all appear at least once.
        for s in &[b"list" as &[u8], b"kv", b"a", b"nk"] {
            assert!(
                encoded.windows(s.len()).any(|w| w == *s),
                "{} should appear in payload",
                std::str::from_utf8(s).unwrap()
            );
        }
    }

    #[test]
    fn promoted_fields_at_payload_level() {
        let payload = TracerPayloadBytes {
            language_name: bs("python"),
            language_version: bs("3.11"),
            tracer_version: bs("2.0.0"),
            runtime_id: bs("rt-1"),
            env: bs("prod"),
            hostname: bs("h"),
            app_version: bs("1.2.3"),
            chunks: vec![make_chunk(vec![make_span("svc", "op", 1)], [0u8; 16])],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        for s in &[
            b"python" as &[u8],
            b"3.11",
            b"2.0.0",
            b"rt-1",
            b"prod",
            b"1.2.3",
        ] {
            assert!(
                encoded.windows(s.len()).any(|w| w == *s),
                "{} should appear",
                std::str::from_utf8(s).unwrap()
            );
        }
    }

    #[test]
    fn chunk_level_attrs_emitted_when_set() {
        let chunk = TraceChunkBytes {
            trace_id: [0u8; 16],
            priority: Some(1),
            origin: Some(bs("lambda")),
            sampling_mechanism: Some(4),
            spans: vec![make_span("svc", "op", 1)],
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        assert!(
            encoded.windows(b"lambda".len()).any(|w| w == b"lambda"),
            "chunk origin should appear"
        );
        // sampling_mechanism=4 → SAMPLING_MECHANISM (0x07) + positive fixint 0x04
        let want = [chunk_key::SAMPLING_MECHANISM, 0x04];
        assert!(encoded.windows(2).any(|w| w == want));
    }

    #[test]
    fn chunk_dropped_trace_emitted_when_true() {
        let chunk = TraceChunkBytes {
            trace_id: [0u8; 16],
            dropped_trace: true,
            spans: vec![make_span("svc", "op", 1)],
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        // DROPPED_TRACE (0x05) + msgpack true marker (0xc3)
        let want = [chunk_key::DROPPED_TRACE, 0xc3];
        assert!(
            encoded.windows(2).any(|w| w == want),
            "DROPPED_TRACE marker + true should appear in payload"
        );
    }

    #[test]
    fn chunk_dropped_trace_skipped_when_false() {
        let chunk = TraceChunkBytes {
            trace_id: [0u8; 16],
            dropped_trace: false,
            spans: vec![make_span("svc", "op", 1)],
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        assert!(
            !encoded.contains(&chunk_key::DROPPED_TRACE),
            "DROPPED_TRACE key should not be emitted when false"
        );
    }

    #[test]
    fn chunk_attributes_emitted_when_set() {
        let mut attrs = std::collections::HashMap::new();
        attrs.insert(bs("region"), AttributeValue::String(bs("us-east-1")));
        let chunk = TraceChunkBytes {
            trace_id: [0u8; 16],
            attributes: attrs,
            spans: vec![make_span("svc", "op", 1)],
            ..Default::default()
        };
        let payload = TracerPayloadBytes {
            chunks: vec![chunk],
            ..Default::default()
        };
        let encoded = to_vec_from_payload(&payload);
        // ATTRIBUTES (0x03) + msgpack fixarray header for 3 elements (0x93)
        let want = [chunk_key::ATTRIBUTES, 0x93];
        assert!(
            encoded.windows(2).any(|w| w == want),
            "ATTRIBUTES key + flat-triplet array header should appear"
        );
        assert!(
            encoded
                .windows(b"us-east-1".len())
                .any(|w| w == b"us-east-1"),
            "chunk attribute value should be in the payload"
        );
    }

    #[test]
    fn span_kind_otel_values() {
        for (kind, expected_byte) in [
            (SpanKind::Internal, 0x01u8),
            (SpanKind::Server, 0x02),
            (SpanKind::Client, 0x03),
            (SpanKind::Producer, 0x04),
            (SpanKind::Consumer, 0x05),
        ] {
            let span = V1Span {
                service: bs("svc"),
                name: bs("op"),
                resource: bs("res"),
                span_id: 1,
                start: 1,
                duration: 1,
                span_kind: kind,
                ..Default::default()
            };
            let payload = TracerPayloadBytes {
                chunks: vec![make_chunk(vec![span], [0u8; 16])],
                ..Default::default()
            };
            let encoded = to_vec_from_payload(&payload);
            let want = [0x10u8, expected_byte];
            assert!(
                encoded.windows(2).any(|w| w == want),
                "SpanKind {kind:?} should produce byte {expected_byte:#x}"
            );
        }
    }

    #[test]
    fn string_interning_works_across_chunks() {
        // The string "shared" appears in two chunks. The second occurrence must be a uint ID,
        // not a fresh str. Compare against a baseline with a single occurrence to verify.
        let chunk_with_two = TracerPayloadBytes {
            chunks: vec![
                make_chunk(vec![make_span("shared", "op1", 1)], [0u8; 16]),
                make_chunk(vec![make_span("shared", "op2", 2)], [0u8; 16]),
            ],
            ..Default::default()
        };
        let single = TracerPayloadBytes {
            chunks: vec![make_chunk(vec![make_span("shared", "op1", 1)], [0u8; 16])],
            ..Default::default()
        };
        let two = to_vec_from_payload(&chunk_with_two);
        let one = to_vec_from_payload(&single);
        assert!(
            two.len() < 2 * one.len(),
            "interning should reduce repeated payload size"
        );
    }
}

#[cfg(test)]
mod cross_validation_tests {
    //! Cross-validates that the M1 encoder (v0.4 spans → V1 payload) and the M3 encoder
    //! (v1::Span → V1 payload) produce **byte-identical** output for equivalent inputs.
    //!
    //! All tests are limited to deterministic content (at most one attribute key per map) so the
    //! `HashMap` iteration order cannot diverge between the two inputs.

    use super::*;
    use crate::span::v04::SpanBytes as V04Span;
    use crate::span::v1::{
        AttributeValue, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes, TracerPayloadBytes,
    };
    use libdd_tinybytes::BytesString;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap_or_default()
    }

    /// Builds a 128-bit big-endian trace_id from `(high, low)` 64-bit halves.
    fn tid_bytes(high: u64, low: u64) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&high.to_be_bytes());
        out[8..].copy_from_slice(&low.to_be_bytes());
        out
    }

    /// Asserts that encoding `v04` (with `metadata`) via M1 produces the same bytes as
    /// encoding `v1` via M3. Includes a hex-diff message on mismatch.
    #[track_caller]
    fn assert_byte_equal(
        v04_traces: &[Vec<V04Span>],
        metadata: &TracerMetadata,
        v1_payload: &TracerPayloadBytes,
    ) {
        let m1 = to_vec(v04_traces, metadata);
        let m3 = to_vec_from_payload(v1_payload);
        if m1 != m3 {
            panic!(
                "M1 and M3 encoders diverged:\n  M1 ({:3} bytes): {}\n  M3 ({:3} bytes): {}",
                m1.len(),
                hex_dump(&m1),
                m3.len(),
                hex_dump(&m3)
            );
        }
    }

    fn hex_dump(b: &[u8]) -> String {
        b.iter().map(|c| format!("{c:02x}")).collect::<String>()
    }

    #[test]
    fn empty_payload_byte_identical() {
        let v04: Vec<Vec<V04Span>> = vec![];
        let v1 = TracerPayloadBytes::default();
        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn minimal_single_span_byte_identical() {
        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 0x42,
            span_id: 1,
            start: 1_000_000,
            duration: 500,
            ..Default::default()
        }]];

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 0x42),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1_000_000,
                    duration: 500,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn span_with_parent_and_error_byte_identical() {
        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 2,
            parent_id: 1,
            start: 1000,
            duration: 100,
            error: 1,
            ..Default::default()
        }]];

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 2,
                    parent_id: 1,
                    start: 1000,
                    duration: 100,
                    error: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn promoted_fields_byte_identical() {
        // M1 reads env/version/component/span.kind from v04 meta and promotes them; M3 takes
        // them directly from the v1::Span fields. Both must produce the same bytes.
        let mut meta = HashMap::new();
        meta.insert(bs("env"), bs("prod"));
        meta.insert(bs("version"), bs("1.2.3"));
        meta.insert(bs("component"), bs("flask"));
        meta.insert(bs("span.kind"), bs("server"));

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        }]];

        // metadata.env populated → M1 picks env from metadata first (it's set on the builder).
        let metadata = TracerMetadata {
            env: "prod".to_string(),
            app_version: "1.2.3".to_string(),
            ..Default::default()
        };

        let v1 = TracerPayloadBytes {
            env: bs("prod"),
            app_version: bs("1.2.3"),
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    env: bs("prod"),
                    version: bs("1.2.3"),
                    component: bs("flask"),
                    span_kind: SpanKind::Server,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &metadata, &v1);
    }

    #[test]
    fn single_string_meta_attribute_byte_identical() {
        // One non-promoted meta tag → one attribute triplet. With a single entry the HashMap
        // iteration order cannot vary.
        let mut meta = HashMap::new();
        meta.insert(bs("custom.tag"), bs("hello"));

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        }]];

        let mut attrs = HashMap::new();
        attrs.insert(bs("custom.tag"), AttributeValue::String(bs("hello")));

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    attributes: attrs,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn single_float_metric_byte_identical() {
        let mut metrics = HashMap::new();
        metrics.insert(bs("score"), 1.5f64);

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            metrics,
            ..Default::default()
        }]];

        let mut attrs = HashMap::new();
        attrs.insert(bs("score"), AttributeValue::Float(1.5));

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    attributes: attrs,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn single_bytes_meta_struct_byte_identical() {
        let mut meta_struct = HashMap::new();
        meta_struct.insert(
            bs("payload"),
            libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad\xbe\xef"),
        );

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            meta_struct,
            ..Default::default()
        }]];

        let mut attrs = HashMap::new();
        attrs.insert(
            bs("payload"),
            AttributeValue::Bytes(libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad\xbe\xef")),
        );

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    attributes: attrs,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn chunk_origin_only_byte_identical() {
        // The M1 encoder's `is_promoted` filter only strips env/version/component/span.kind/
        // _dd.p.tid — it intentionally keeps `_dd.origin` in span attributes even though it's
        // also lifted to the chunk. M3 must reproduce that duplication for byte equality.
        let mut meta = HashMap::new();
        meta.insert(bs("_dd.origin"), bs("lambda"));

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        }]];

        let mut attrs = HashMap::new();
        attrs.insert(bs("_dd.origin"), AttributeValue::String(bs("lambda")));
        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                origin: Some(bs("lambda")),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    attributes: attrs,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn trace_id_128_bit_from_dd_p_tid_byte_identical() {
        let mut meta = HashMap::new();
        meta.insert(bs("_dd.p.tid"), bs("640cfd5400000000"));

        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 0x0123456789abcdef,
            span_id: 1,
            start: 1000,
            duration: 100,
            meta,
            ..Default::default()
        }]];

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0x640cfd5400000000, 0x0123456789abcdef),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn tracer_metadata_fields_byte_identical() {
        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            ..Default::default()
        }]];
        let metadata = TracerMetadata {
            language: "python".to_string(),
            language_version: "3.11".to_string(),
            tracer_version: "2.0.0".to_string(),
            runtime_id: "abc-uuid".to_string(),
            hostname: "h1".to_string(),
            ..Default::default()
        };

        let v1 = TracerPayloadBytes {
            language_name: bs("python"),
            language_version: bs("3.11"),
            tracer_version: bs("2.0.0"),
            runtime_id: bs("abc-uuid"),
            hostname: bs("h1"),
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &metadata, &v1);
    }

    #[test]
    fn payload_attribute_git_commit_sha_byte_identical() {
        let v04 = vec![vec![V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            ..Default::default()
        }]];
        let metadata = TracerMetadata {
            git_commit_sha: "abc123".to_string(),
            ..Default::default()
        };

        let mut payload_attrs = HashMap::new();
        payload_attrs.insert(
            bs("_dd.git.commit.sha"),
            AttributeValue::String(bs("abc123")),
        );

        let v1 = TracerPayloadBytes {
            attributes: payload_attrs,
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &metadata, &v1);
    }

    #[test]
    fn span_with_single_link_byte_identical() {
        let v04_span = V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            span_links: vec![crate::span::v04::SpanLink {
                trace_id: 0x0123456789abcdef,
                trace_id_high: 0,
                span_id: 99,
                tracestate: bs("running"),
                flags: 0,
                attributes: HashMap::new(),
            }],
            ..Default::default()
        };
        let v04 = vec![vec![v04_span]];

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    span_links: vec![crate::span::v1::SpanLinkBytes {
                        trace_id: tid_bytes(0, 0x0123456789abcdef),
                        span_id: 99,
                        tracestate: bs("running"),
                        flags: 0,
                        attributes: HashMap::new(),
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }

    #[test]
    fn span_with_single_event_byte_identical() {
        use crate::span::v04::{AttributeAnyValue, AttributeArrayValue};

        let v04_span = V04Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            trace_id: 1,
            span_id: 1,
            start: 1000,
            duration: 100,
            span_events: vec![crate::span::v04::SpanEvent {
                time_unix_nano: 42,
                name: bs("exception"),
                attributes: HashMap::from([(
                    bs("exception.message"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::String(bs("boom"))),
                )]),
            }],
            ..Default::default()
        };
        let v04 = vec![vec![v04_span]];

        let v1 = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 1),
                spans: vec![V1SpanBytes {
                    service: bs("svc"),
                    name: bs("op"),
                    resource: bs("res"),
                    span_id: 1,
                    start: 1000,
                    duration: 100,
                    span_events: vec![crate::span::v1::SpanEventBytes {
                        time_unix_nano: 42,
                        name: bs("exception"),
                        attributes: HashMap::from([(
                            bs("exception.message"),
                            AttributeValue::String(bs("boom")),
                        )]),
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_byte_equal(&v04, &TracerMetadata::default(), &v1);
    }
}
