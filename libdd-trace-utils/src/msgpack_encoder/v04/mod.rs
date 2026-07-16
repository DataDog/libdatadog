// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v04::Span;
use crate::span::v1::TracerPayload;
use crate::span::TraceData;
use libdd_common::ResultInfallibleExt;
use rmp::encode::{write_array_len, ByteBuf, RmpWrite, ValueWriteError};

const fn msgpack_string_encoding_len(s: &str) -> usize {
    const U16_MAX: usize = u16::MAX as usize;
    let length_marker_len = match s.len() {
        0..32 => 1,
        32..256 => 2,
        256..=U16_MAX => 3,
        _ => 5,
    };
    length_marker_len + s.len()
}

// Compute the encoding of a string to msgpack in a const manner
const fn msgpack_const_string_encoding<const ENCODING_LEN: usize>(s: &str) -> [u8; ENCODING_LEN] {
    // copy_to_slice is not const yet, so we make a helper
    const fn copy_to_slice(dest: &mut [u8], src: &[u8], n: usize) {
        let mut i = 0;
        while i < n {
            dest[i] = src[i];
            i += 1;
        }
    }

    let mut storage = [0; ENCODING_LEN];
    let len = s.len() as u64;
    let len_bytes = if len < 32 {
        storage[0] = 0xa0 | (len as u8 & 0x1f);
        0
    } else if len < 256 {
        storage[0] = 0xd9;
        1
    } else if len <= (u16::MAX as u64) {
        storage[0] = 0xda;
        2
    } else {
        storage[0] = 0xdb;
        4
    };
    let len_be_bytes = len.to_be_bytes();
    // `len_be_bytes` holds `len` as 8 big-endian bytes; the marker only needs the low-order
    // `len_bytes` of those (e.g. for a str8 length of 200, that's byte `[200]`, not `[0]`), so
    // skip the leading zero bytes rather than copying from the front.
    copy_to_slice(
        storage.split_at_mut(1).1,
        len_be_bytes.split_at(8 - len_bytes).1,
        len_bytes,
    );
    copy_to_slice(storage.split_at_mut(1 + len_bytes).1, s.as_bytes(), s.len());
    storage
}

macro_rules! write_const_msgpack_str {
    ($writer:expr, $str:expr) => {{
        use rmp::encode::ValueWriteError;
        const STRING_ENCODING_LEN: usize = super::msgpack_string_encoding_len($str);
        const STRING_ENCODING: [u8; STRING_ENCODING_LEN] =
            super::msgpack_const_string_encoding($str);

        $writer
            .write_bytes(&STRING_ENCODING)
            .map_err(ValueWriteError::InvalidDataWrite)
    }};
}

mod span_v04;
mod span_v1;

#[inline(always)]
fn to_writer<W: RmpWrite, T: TraceData, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    traces: &[S],
) -> Result<(), ValueWriteError<W::Error>> {
    write_array_len(writer, traces.len() as u32)?;
    for trace in traces {
        write_array_len(writer, trace.as_ref().len() as u32)?;
        for span in trace.as_ref() {
            span_v04::encode_span(writer, span)?;
        }
    }

    Ok(())
}

/// Encodes a collection of traces into a slice of bytes.
///
/// # Arguments
///
/// * `slice` - A mutable reference to a byte slice.
/// * `traces` - A reference to a slice of spans.
///
/// # Returns
///
/// * `Ok(())` - If encoding succeeds.
/// * `Err(ValueWriteError)` - If encoding fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The array length for trace count or span count cannot be written.
/// - Any span cannot be encoded.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v04::write_to_slice_from_v04;
/// use libdd_trace_utils::span::v04::SpanSlice;
///
/// let mut buffer = vec![0u8; 1024];
/// let span = SpanSlice {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
///
/// write_to_slice_from_v04(&mut &mut buffer[..], &traces).expect("Encoding failed");
/// ```
pub fn write_to_slice_from_v04<T: TraceData, S: AsRef<[Span<T>]>>(
    slice: &mut &mut [u8],
    traces: &[S],
) -> Result<(), ValueWriteError> {
    to_writer(slice, traces)
}

/// Serializes traces into a vector of bytes with a default capacity of 0.
///
/// # Arguments
///
/// * `traces` - A reference to a slice of spans.
///
/// # Returns
///
/// * `Vec<u8>` - A vector containing encoded traces.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v04::to_vec_from_v04;
/// use libdd_trace_utils::span::v04::SpanSlice;
///
/// let span = SpanSlice {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded = to_vec_from_v04(&traces);
///
/// assert!(!encoded.is_empty());
/// ```
pub fn to_vec_from_v04<T: TraceData, S: AsRef<[Span<T>]>>(traces: &[S]) -> Vec<u8> {
    to_vec_with_capacity_from_v04(traces, 0)
}

/// Serializes traces into a vector of bytes with specified capacity.
///
/// # Arguments
///
/// * `traces` - A reference to a slice of spans.
/// * `capacity` - Desired initial capacity of the resulting vector.
///
/// # Returns
///
/// * `Vec<u8>` - A vector containing encoded traces.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v04::to_vec_with_capacity_from_v04;
/// use libdd_trace_utils::span::v04::SpanSlice;
///
/// let span = SpanSlice {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded = to_vec_with_capacity_from_v04(&traces, 1024);
///
/// assert!(encoded.capacity() >= 1024);
/// ```
pub fn to_vec_with_capacity_from_v04<T: TraceData, S: AsRef<[Span<T>]>>(
    traces: &[S],
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    to_writer(&mut buf, traces)
        .map_err(super::flatten_value_write_infallible)
        .unwrap_infallible();
    buf.into_vec()
}

/// Computes the number of bytes required to encode the given traces.
///
/// This does not allocate any actual buffer, but simulates writing in order to measure
/// the encoded size of the traces.
///
/// # Arguments
///
/// * `traces` - A reference to a slice of spans.
///
/// # Returns
///
/// * `u32` - The number of bytes that would be written by the encoder.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::msgpack_encoder::v04::to_encoded_byte_len_from_v04;
/// use libdd_trace_utils::span::v04::SpanSlice;
///
/// let span = SpanSlice {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded_len = to_encoded_byte_len_from_v04(&traces);
///
/// assert!(encoded_len > 0);
/// ```
pub fn to_encoded_byte_len_from_v04<T: TraceData, S: AsRef<[Span<T>]>>(traces: &[S]) -> u32 {
    let mut counter = super::CountLength(0);
    // `CountLength` impls `std::io::Write` (whose error type is `std::io::Error`, not
    // `Infallible`), so we can't statically prove infallibility via `unwrap_infallible`
    // the way we do for `ByteBuf`. In practice `CountLength::write*` only ever return
    // `Ok`, so the error path here is unreachable today; should `CountLength` ever grow
    // a fallible code path, fuzz tests on the msgpack encoded length would catch it.
    let _ = to_writer(&mut counter, traces);
    counter.0
}

/// Encodes a [`TracerPayload`] in the v0.4 wire format (downgrade path used when the agent
/// does not advertise `/v1.0/traces`). The output is a msgpack array of traces, where each
/// trace is itself a msgpack array of v0.4-shaped spans — matching the existing v0.4 wire
/// format produced by [`to_vec_from_v04`]. Payload-level `env`/`app_version`/`attributes` are
/// propagated into every span (see [`span_v1`]'s mapping table); `payload.hostname` has no v0.4
/// body equivalent (the agent gets hostname from the `Datadog-Meta-Hostname` header instead) and is
/// intentionally dropped here.
fn encode_payload_from_v1<W: RmpWrite, T: TraceData>(
    writer: &mut W,
    payload: &TracerPayload<T>,
) -> Result<(), ValueWriteError<W::Error>> {
    use span_v1::{encode_span, ChunkContext};

    write_array_len(writer, payload.chunks.len() as u32)?;
    for chunk in &payload.chunks {
        // v0.4 has no wire-level equivalent of `dropped_trace`; the closest historical signal
        // is `USER_REJECT` (priority -1), which tells the agent the sampler rejected this trace
        // without dropping the spans themselves. Only force it when the chunk doesn't already
        // carry a negative (reject-like) priority.
        let priority = if chunk.dropped_trace {
            Some(chunk.priority.filter(|&p| p < 0).unwrap_or(-1))
        } else {
            chunk.priority
        };
        let ctx = ChunkContext::new(
            &chunk.trace_id,
            priority,
            &chunk.origin,
            chunk.sampling_mechanism,
            &chunk.attributes,
            &payload.env,
            &payload.app_version,
            &payload.attributes,
        );
        write_array_len(writer, chunk.spans.len() as u32)?;
        for span in &chunk.spans {
            encode_span(writer, span, &ctx)?;
        }
    }
    Ok(())
}

/// Serializes a [`TracerPayload`] (V1 data model) as a v0.4 msgpack payload.
///
/// Used by the trace exporter when the agent has not advertised `/v1.0/traces` via `/info`.
/// The output is byte-compatible with [`to_vec_from_v04`] for equivalent data — chunk-level fields
/// are propagated to every span and typed attributes are bucketed into the v0.4 `meta` /
/// `metrics` / `meta_struct` maps per [`span_v1`]'s mapping table.
pub fn to_vec_from_v1<T: TraceData>(payload: &TracerPayload<T>) -> Vec<u8> {
    to_vec_with_capacity_from_v1(payload, 0)
}

/// Serializes a [`TracerPayload`] as a v0.4 msgpack payload with a caller-supplied initial
/// capacity. Use this when you can size the buffer up front (e.g. from
/// [`to_encoded_byte_len_from_v1`]) to avoid reallocations.
pub fn to_vec_with_capacity_from_v1<T: TraceData>(
    payload: &TracerPayload<T>,
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    encode_payload_from_v1(&mut buf, payload)
        .map_err(super::flatten_value_write_infallible)
        .unwrap_infallible();
    buf.into_vec()
}

/// Encodes a [`TracerPayload`] as v0.4 msgpack into the provided slice. Useful for callers
/// that own a pre-sized buffer (e.g. for FFI / zero-copy paths).
///
/// # Errors
///
/// Returns any [`ValueWriteError`] from the underlying writer (typically buffer-too-small).
pub fn write_to_slice_from_v1<T: TraceData>(
    slice: &mut &mut [u8],
    payload: &TracerPayload<T>,
) -> Result<(), ValueWriteError> {
    encode_payload_from_v1(slice, payload)
}

/// Returns the exact number of bytes [`to_vec_from_v1`] would write for `payload`. Walks
/// the payload through a counting writer without allocating an output buffer.
pub fn to_encoded_byte_len_from_v1<T: TraceData>(payload: &TracerPayload<T>) -> u32 {
    let mut counter = super::CountLength(0);
    let _ = encode_payload_from_v1(&mut counter, payload);
    counter.0
}

#[cfg(test)]
mod tests {
    //! Regression tests for [`msgpack_const_string_encoding`] across every msgpack string
    //! length-marker boundary (fixstr / str8 / str16), since the length only fits in the
    //! low-order bytes of `len.to_be_bytes()` and it's easy to accidentally copy from the
    //! high-order (zero) end instead.
    use super::msgpack_const_string_encoding;

    fn encode<const N: usize>(s: &str) -> [u8; N] {
        msgpack_const_string_encoding::<N>(s)
    }

    #[test]
    fn fixstr_boundary_31_bytes() {
        let s = "a".repeat(31);
        let bytes: [u8; 32] = encode(&s);
        let value = rmpv::decode::read_value(&mut &bytes[..]).expect("decode failed");
        assert_eq!(value.as_str(), Some(s.as_str()));
    }

    #[test]
    fn str8_boundary_200_bytes() {
        let s = "b".repeat(200);
        let bytes: [u8; 202] = encode(&s);
        let value = rmpv::decode::read_value(&mut &bytes[..]).expect("decode failed");
        assert_eq!(value.as_str(), Some(s.as_str()));
    }

    #[test]
    fn str16_boundary_300_bytes() {
        let s = "c".repeat(300);
        let bytes: [u8; 303] = encode(&s);
        let value = rmpv::decode::read_value(&mut &bytes[..]).expect("decode failed");
        assert_eq!(value.as_str(), Some(s.as_str()));
    }
}
