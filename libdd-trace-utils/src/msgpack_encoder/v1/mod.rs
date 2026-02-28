// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v1::TracePayload;
use crate::span::TraceData;
use rmp::encode::{ByteBuf, RmpWrite, ValueWriteError};
use crate::msgpack_encoder::v1::trace::TraceEncoder;
use super::CountLength;

mod trace;

#[inline(always)]
fn to_writer<W: RmpWrite, T: TraceData>(writer: &mut W, trace_payload: &TracePayload<T>) -> Result<(), ValueWriteError<W::Error>> {
    TraceEncoder::new(writer, &trace_payload.static_data).encode_traces(&trace_payload.traces)
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
/// use libdd_trace_utils::msgpack_encoder::v1::write_to_slice;
/// use libdd_trace_utils::span::{BytesData, v1::TracePayload};
///
/// let traces = TracePayload::<BytesData>::default();
///
/// write_to_slice(&mut &mut buffer[..], &traces).expect("Encoding failed");
/// ```
pub fn write_to_slice<T: TraceData>(
    slice: &mut &mut [u8],
    trace_payload: &TracePayload<T>,
) -> Result<(), ValueWriteError> {
    to_writer(slice, trace_payload)
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
/// use libdd_trace_utils::msgpack_encoder::v1::write_to_slice;
/// use libdd_trace_utils::span::{BytesData, v1::TracePayload};
///
/// let traces = TracePayload::<BytesData>::default();
/// let encoded = to_vec(&traces);
///
/// assert!(!encoded.is_empty());
/// ```
pub fn to_vec<T: TraceData>(trace_payload: &TracePayload<T>) -> Vec<u8> {
    to_vec_with_capacity(trace_payload, 0)
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
/// use libdd_trace_utils::msgpack_encoder::v1::write_to_slice;
/// use libdd_trace_utils::span::{BytesData, v1::TracePayload};
///
/// let traces = TracePayload::<BytesData>::default();
/// let encoded = to_vec_with_capacity(&traces, 1024);
///
/// assert!(encoded.capacity() >= 1024);
/// ```
pub fn to_vec_with_capacity<T: TraceData>(
    trace_payload: &TracePayload<T>,
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    #[allow(clippy::expect_used)]
    to_writer(&mut buf, trace_payload).expect("infallible: the error is std::convert::Infallible");
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
/// use libdd_trace_utils::msgpack_encoder::v1::write_to_slice;
/// use libdd_trace_utils::span::{BytesData, v1::TracePayload};
///
/// let traces = TracePayload::<BytesData>::default();
/// let encoded_len = to_len(&traces);
///
/// assert!(encoded_len > 0);
/// ```
pub fn to_len<T: TraceData>(trace_payload: &TracePayload<T>) -> u32 {
    let mut counter = CountLength(0);
    #[allow(clippy::expect_used)]
    to_writer(&mut counter, trace_payload).expect("infallible: CountLength never fails");
    counter.0
}
