// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::io;

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
        write_trace(writer, trace)?;
    }

    Ok(())
}

#[inline(always)]
fn write_trace<W: RmpWrite, T: SpanText, S: AsRef<[Span<T>]>>(
    writer: &mut W,
    trace: &S,
) -> Result<(), ValueWriteError<W::Error>> {
    write_array_len(writer, trace.as_ref().len() as u32)?;
    for span in trace.as_ref() {
        span::encode_span(writer, span)?;
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
/// use libdd_trace_utils::msgpack_encoder::v04::write_to_slice;
/// use libdd_trace_utils::span::Span;
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
/// use libdd_trace_utils::msgpack_encoder::v04::to_vec;
/// use libdd_trace_utils::span::Span;
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
/// use libdd_trace_utils::msgpack_encoder::v04::to_vec_with_capacity;
/// use libdd_trace_utils::span::Span;
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
    unwrap_infallible_write(to_writer(&mut buf, traces));
    buf.into_vec()
}

const ARRAY_LEN_HEADER_WIDTH: usize = 5;

pub struct TraceBuffer {
    buf: Vec<u8>,
    trace_count: usize,
    max_trace_size: usize,
    max_size: usize,
}

fn mp_write_array_len_fixed_width(buf: &mut [u8], len: usize) {
    if buf.len() < ARRAY_LEN_HEADER_WIDTH {
        return;
    }
    let Ok(len) = u32::try_from(len) else { return };
    buf[0] = 0xdd;
    let len_encoded: [u8; 4] = len.to_be_bytes();
    buf[1..5].copy_from_slice(&len_encoded);
}

struct LimitedTruncatingWriter<'a> {
    w: &'a mut Vec<u8>,
    written: usize,
    limit: usize,
}

impl LimitedTruncatingWriter<'_> {
    fn rollback(&mut self) {
        self.w.truncate(self.w.len() - self.written);
        self.written = 0;
    }
}

impl io::Write for LimitedTruncatingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "no space left in the buffer",
            ));
        }
        let written = self.w.write(buf)?;
        self.written += written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

impl TraceBuffer {
    pub fn new(max_trace_size: usize, max_size: usize) -> Self {
        Self {
            buf: vec![0; ARRAY_LEN_HEADER_WIDTH],
            trace_count: 0,
            max_trace_size,
            max_size,
        }
    }

    fn writer(&mut self) -> LimitedTruncatingWriter<'_> {
        let leftover = self.max_size.saturating_sub(self.buf.len());
        LimitedTruncatingWriter {
            w: &mut self.buf,
            written: 0,
            limit: self.max_trace_size.min(leftover),
        }
    }

    pub fn write_trace<T: SpanText, S: AsRef<[Span<T>]>>(&mut self, trace: S) -> io::Result<()> {
        let mut writer = self.writer();
        match write_trace(&mut writer, &trace) {
            Ok(()) => {}
            Err(ValueWriteError::InvalidDataWrite(e) | ValueWriteError::InvalidMarkerWrite(e)) => {
                writer.rollback();
                return Err(e);
            }
        };
        self.trace_count += 1;
        Ok(())
    }

    fn reset(&mut self) -> Vec<u8> {
        let buf = std::mem::take(&mut self.buf);
        *self = Self {
            buf: {
                let mut v = Vec::with_capacity(buf.len());
                v.resize(ARRAY_LEN_HEADER_WIDTH, 0);
                v
            },
            trace_count: 0,
            max_trace_size: self.max_trace_size,
            max_size: self.max_size,
        };
        buf
    }

    pub fn flush(&mut self) -> Vec<u8> {
        self.write_traces_len();
        let buf = self.reset();
        buf
    }

    fn write_traces_len(&mut self) {
        mp_write_array_len_fixed_width(&mut self.buf, self.trace_count);
    }
}

/// Serializes traces into a vector of bytes passed mutably
pub fn to_vec_extend<T: SpanText, S: AsRef<[Span<T>]>>(traces: &[S], v: &mut Vec<u8>) {
    let mut buf = ByteBuf::from_vec(std::mem::take(v));
    #[allow(clippy::expect_used)]
    unwrap_infallible_write(to_writer(&mut buf, traces));
    *v = buf.into_vec();
}

/// Unwrap an infallible result without panics
fn unwrap_infallible_write<T>(res: Result<T, ValueWriteError<std::convert::Infallible>>) -> T {
    match res {
        Ok(ok) => ok,
        Err(e) => match match e {
            ValueWriteError::InvalidMarkerWrite(i) => i,
            ValueWriteError::InvalidDataWrite(i) => i,
        } {},
    }
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
/// use libdd_trace_utils::msgpack_encoder::v04::to_len;
/// use libdd_trace_utils::span::Span;
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
