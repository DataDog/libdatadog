// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::{BytesData, DeserializableTraceData, SliceData};
use rmp::decode;
use rmp::decode::DecodeStringError;
use rmpv::decode::value_ref::BorrowRead;
use std::io::Read;
use std::ops::Deref;

/// Read a string from `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string.
#[inline]
pub fn read_string_ref_nomut(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
    decode::read_str_from_slice(buf).map_err(|e| match e {
        DecodeStringError::InvalidMarkerRead(e) => DecodeError::InvalidFormat(e.to_string()),
        DecodeStringError::InvalidDataRead(e) => DecodeError::InvalidConversion(e.to_string()),
        DecodeStringError::TypeMismatch(marker) => {
            DecodeError::InvalidType(format!("Type mismatch at marker {marker:?}"))
        }
        DecodeStringError::InvalidUtf8(_, e) => DecodeError::Utf8Error(e.to_string()),
        _ => DecodeError::IOError,
    })
}

/// Internal buffer used to decode a msgpack payload.
///
/// `source` borrows the full input backing store. It never advances and is used to derive
/// decoded strings and byte ranges with the correct ownership (`BytesString` / refcounted
/// slicing for `BytesData`, borrowed values for `SliceData`). `reader` is the single source
/// of decoding position over that input.
///
/// The buffer exposes two reading surfaces over the same position:
/// - [`Buffer::as_mut_slice`]: `rmp` implements its sealed [`rmp::decode::RmpRead`] natively for
///   the underlying zero-copy reader, which is what the decode hot path uses.
/// - [`std::io::Read`] and [`BorrowRead`], so [`rmpv::decode::read_value_ref`] can decode a whole
///   value from the buffer while borrowing strings directly from the payload at its honest lifetime
///   (no `'static` lie, no unsafe).
///
/// `T: DeserializableTraceData<'a>` ties `'a` to the mode's input source: `&'a [u8]` for
/// `SliceData<'a>`, `&'a Bytes` for `BytesData`.
pub struct Buffer<'a, T: DeserializableTraceData<'a>> {
    source: &'a T::Source,
    /// `rmp`'s reader over the remaining bytes. Its position is only used for rmp's error
    /// messages, which the decoder discards.
    reader: decode::Bytes<'a>,
}

impl<'a> From<&'a [u8]> for Buffer<'a, SliceData<'a>> {
    fn from(source: &'a [u8]) -> Self {
        Self {
            source,
            reader: decode::Bytes::new(source),
        }
    }
}

impl<'a> From<&'a libdd_tinybytes::Bytes> for Buffer<'a, BytesData> {
    fn from(source: &'a libdd_tinybytes::Bytes) -> Self {
        Self {
            source,
            reader: decode::Bytes::new(source.as_ref()),
        }
    }
}

impl<'a, T: DeserializableTraceData<'a>> Buffer<'a, T> {
    /// Returns a mutable handle to the buffer's `rmp` reader. `rmp`'s read functions advance
    /// it in place (zero copy), which advances the buffer.
    pub fn as_mut_slice(&mut self) -> &mut decode::Bytes<'a> {
        &mut self.reader
    }

    /// Returns the full input backing store.
    #[must_use]
    pub fn source(&self) -> &'a T::Source {
        self.source
    }

    /// Returns the unconsumed bytes at the payload's honest lifetime without advancing.
    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        self.reader.remaining_slice()
    }

    /// Advances the buffer by `bytes` without producing a value. Returns `false` (and leaves
    /// the buffer untouched) if fewer than `bytes` bytes remain.
    pub fn advance(&mut self, bytes: usize) -> bool {
        match self.remaining().get(bytes..) {
            Some(rest) => {
                // Rebuild the reader over the new position: O(1), no data movement. Its
                // position (only used for rmp error reporting, which the decoder discards)
                // resets to zero.
                self.reader = decode::Bytes::new(rest);
                true
            }
            None => false,
        }
    }

    /// Tries to extract a slice of `bytes` from the buffer and advances the buffer.
    pub fn try_slice_and_advance(&mut self, bytes: usize) -> Option<T::Bytes> {
        T::try_slice_and_advance(self, bytes)
    }

    /// Read a string from the slices `buf`.
    ///
    /// # Errors
    /// Fails if the buffer doesn't contain a valid utf8 msgpack string.
    pub fn read_string(&mut self) -> Result<T::Text, DecodeError> {
        T::read_string(self)
    }

    /// Caps a decoded element count at the bytes remaining in the buffer. Each msgpack
    /// element needs >=1 byte on the wire, so a length prefix can't legitimately exceed
    /// the remaining bytes — this prevents a malicious count (e.g. 0xFFFFFFFF) from
    /// forcing a huge pre-allocation before any element is read.
    pub fn capped_capacity(&self, count: usize) -> usize {
        count.min(self.len())
    }
}

/// Required by rmpv's [`BorrowRead`] supertrait and by rmp's sealed `RmpRead` blanket
/// implementation for the marker and scalar reads performed inside `read_value_ref`.
impl<'a, T: DeserializableTraceData<'a>> Read for Buffer<'a, T> {
    #[inline]
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.remaining();
        let n = out.len().min(remaining.len());
        out[..n].copy_from_slice(&remaining[..n]);
        self.advance(n);
        Ok(n)
    }
}

/// Lets rmpv decode a zero-copy [`rmpv::ValueRef`] directly from the buffer.
impl<'a, T: DeserializableTraceData<'a>> BorrowRead<'a> for Buffer<'a, T> {
    #[inline]
    fn fill_buf(&self) -> &'a [u8] {
        self.remaining()
    }

    #[inline]
    fn consume(&mut self, len: usize) {
        // rmpv only consumes lengths it has first checked against `fill_buf`, so the advance
        // always succeeds here.
        self.advance(len);
    }
}

impl<'a, T: DeserializableTraceData<'a>> Deref for Buffer<'a, T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.remaining()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_advance_respects_bounds() {
        let bytes = [1, 2, 3];
        let mut buffer = Buffer::<SliceData>::from(&bytes[..]);
        assert!(buffer.advance(2));
        assert_eq!(buffer.remaining(), &[3][..]);
        assert!(!buffer.advance(2), "advancing past the end must fail");
        assert_eq!(
            buffer.remaining(),
            &[3][..],
            "and leave the buffer untouched"
        );
        assert!(buffer.advance(1));
        assert!(buffer.remaining().is_empty());
    }

    #[test]
    fn buffer_is_usable_by_rmp_and_rmpv() {
        // fixstr "hi" followed by a positive fixint 42: rmpv must decode the string straight
        // from the buffer (borrowed, no copy) and rmp must read the int after it.
        let bytes = [0xa2, b'h', b'i', 42];
        let mut buffer = Buffer::<SliceData>::from(&bytes[..]);
        let value =
            rmpv::decode::read_value_ref(&mut buffer).expect("rmpv must read from the buffer");
        assert_eq!(value, rmpv::ValueRef::String("hi".into()));
        let n: u8 = decode::read_int(buffer.as_mut_slice())
            .expect("rmp must read the int from the buffer's reader");
        assert_eq!(n, 42);
        assert!(buffer.remaining().is_empty());
    }

    #[test]
    fn rmp_reader_and_borrow_read_advance_the_same_buffer() {
        // Both reading surfaces must share the buffer's position: a marker read through the
        // rmp reader must be visible to `remaining` (and vice versa).
        let bytes = [0xc0, 42];
        let mut buffer = Buffer::<SliceData>::from(&bytes[..]);
        let marker = decode::read_marker(buffer.as_mut_slice()).expect("marker read");
        assert_eq!(marker, rmp::Marker::Null);
        assert_eq!(buffer.remaining(), &[42]);
    }
}
