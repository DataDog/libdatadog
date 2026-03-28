// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod trace;

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::v1::trace::decode_traces;
use crate::span::{v1::TracePayloadBytes, v1::TracePayloadSlice, DeserializableTraceData};
use crate::span::v1::TracePayload;

/// Decodes a Bytes buffer a `TracePayload` object.
///
/// # Arguments
///
/// * `data` - A libdd_tinybytes Bytes buffer containing the encoded data. Bytes are expected to be
///   encoded msgpack data containing a list of a list of v1 spans.
///
/// # Returns
///
/// * `Ok(TracerPayload, usize)` - A `TracePayload` if successful. and the number of bytes in the
///   slice used by the decoder.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The array length for trace count or span count cannot be read.
/// - Any span cannot be decoded.
///
/// # Examples
///
/// ```
/// use datadog_trace_protobuf::pb::Span;
/// use datadog_trace_utils::msgpack_decoder::v1::from_bytes;
/// use rmp_serde::to_vec_named;
/// use libdd_tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_libdd_tinybytes = libdd_tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_bytes(encoded_data_as_libdd_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name.as_str());
/// ```
pub fn from_bytes(data: libdd_tinybytes::Bytes) -> Result<(TracePayloadBytes, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

/// Decodes a slice of bytes into a `TracePayload` object.
/// The resulting spans have the same lifetime as the initial buffer.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing the encoded data. Bytes are expected to be encoded
///   msgpack data containing a list of a list of v1 spans.
///
/// # Returns
///
/// * `Ok(TracePayload, usize)` - A decoded `TracePayload` object if successful. and the
///   number of bytes in the slice used by the decoder.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The array length for trace count or span count cannot be read.
/// - Any span cannot be decoded.
///
/// # Examples
///
/// ```
/// use datadog_trace_protobuf::pb::Span;
/// use datadog_trace_utils::msgpack_decoder::v1::from_slice;
/// use rmp_serde::to_vec_named;
/// use libdd_tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_libdd_tinybytes = libdd_tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_slice(&encoded_data_as_libdd_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name);
/// ```
pub fn from_slice(data: &[u8]) -> Result<(TracePayloadSlice<'_>, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

#[allow(clippy::type_complexity)]
pub fn from_buffer<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
) -> Result<(TracePayload<T>, usize), DecodeError> {
    let start_len = data.len();

    let mut traces = TracePayload::default();
    decode_traces(data, &mut traces.static_data, &mut traces.traces)?;

    Ok((traces, start_len - data.len()))
}
