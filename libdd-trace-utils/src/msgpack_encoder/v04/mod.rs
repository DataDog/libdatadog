// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{Span, SpanText};
use rmp::encode::{write_array_len, ByteBuf, RmpWrite, ValueWriteError};

mod span;

#[inline(always)]
fn to_writer<W: RmpWrite, T: SpanText, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    traces: &[S],
) -> Result<(), ValueWriteError<W::Error>> {
    write_array_len(writer, traces.len() as u32)?;
    for trace in traces {
        write_array_len(writer, trace.as_ref().len() as u32)?;
        for span in trace.as_ref() {
            span::encode_span(writer, span)?;
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
/// use datadog_trace_utils::msgpack_encoder::v04::write_to_slice;
/// use datadog_trace_utils::span::Span;
///
/// let mut buffer = vec![0u8; 1024];
/// let span = Span {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
///
/// write_to_slice(&mut &mut buffer[..], &traces).expect("Encoding failed");
/// ```
pub fn write_to_slice<T: SpanText, S: AsRef<[Span<T>]>>(
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
/// use datadog_trace_utils::msgpack_encoder::v04::to_vec;
/// use datadog_trace_utils::span::Span;
///
/// let span = Span {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded = to_vec(&traces);
///
/// assert!(!encoded.is_empty());
/// ```
pub fn to_vec<T: SpanText, S: AsRef<[Span<T>]>>(traces: &[S]) -> Vec<u8> {
    to_vec_with_capacity(traces, 0)
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
/// use datadog_trace_utils::msgpack_encoder::v04::to_vec_with_capacity;
/// use datadog_trace_utils::span::Span;
///
/// let span = Span {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded = to_vec_with_capacity(&traces, 1024);
///
/// assert!(encoded.capacity() >= 1024);
/// ```
pub fn to_vec_with_capacity<T: SpanText, S: AsRef<[Span<T>]>>(
    traces: &[S],
    capacity: u32,
) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    #[allow(clippy::expect_used)]
    to_writer(&mut buf, traces).expect("infallible: the error is std::convert::Infallible");
    buf.into_vec()
}

struct CountLength(u32);

impl std::io::Write for CountLength {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0 += buf.len() as u32;
        Ok(())
    }
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
/// use datadog_trace_utils::msgpack_encoder::v04::to_len;
/// use datadog_trace_utils::span::Span;
///
/// let span = Span {
///     name: "test-span",
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
/// let encoded_len = to_len(&traces);
///
/// assert!(encoded_len > 0);
/// ```
pub fn to_len<T: SpanText, S: AsRef<[Span<T>]>>(traces: &[S]) -> u32 {
    let mut counter = CountLength(0);
    #[allow(clippy::expect_used)]
    to_writer(&mut counter, traces).expect("infallible: CountLength never fails");
    counter.0
}
