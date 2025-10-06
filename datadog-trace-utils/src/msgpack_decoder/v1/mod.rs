// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod span;

use self::span::decode_span;
use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::{TraceData, v1::Traces, v1::TracePayloadBytes, v1::TracePayloadSlice};
use crate::span::v1::TracePayload;

/// Decodes a Bytes buffer into a `Vec<Vec<SpanBytes>>` object, also represented as a vector of
/// `TracerPayloadV04` objects.
///
/// # Arguments
///
/// * `data` - A tinybytes Bytes buffer containing the encoded data. Bytes are expected to be
///   encoded msgpack data containing a list of a list of v04 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV04>, usize)` - A vector of decoded `Vec<SpanSlice>` objects if
///   successful. and the number of bytes in the slice used by the decoder.
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
/// use datadog_trace_utils::msgpack_decoder::v04::from_bytes;
/// use rmp_serde::to_vec_named;
/// use tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_tinybytes = tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_bytes(encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name.as_str());
/// ```
pub fn from_bytes(data: tinybytes::Bytes) -> Result<(TracePayloadBytes, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

/// Decodes a slice of bytes into a `Vec<Vec<SpanSlice>>` object.
/// The resulting spans have the same lifetime as the initial buffer.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing the encoded data. Bytes are expected to be encoded
///   msgpack data containing a list of a list of v04 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV04>, usize)` - A vector of decoded `Vec<SpanSlice>` objects if
///   successful. and the number of bytes in the slice used by the decoder.
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
/// use datadog_trace_utils::msgpack_decoder::v04::from_slice;
/// use rmp_serde::to_vec_named;
/// use tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_tinybytes = tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_slice(&encoded_data_as_tinybytes).expect("Decoding failed");
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
pub fn from_buffer<T: TraceData>(
    data: &mut Buffer<T>,
) -> Result<(TracePayload<T>, usize), DecodeError> {
    let trace_count = rmp::decode::read_array_len(data.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read array len for trace count".to_owned())
    })?;

    let traces = TracePayload::default();

    // Intentionally skip the size of the array (as it will be recomputed after coalescing).
    let start_len = data.len();



    Ok((traces, start_len - data.len()))
}
