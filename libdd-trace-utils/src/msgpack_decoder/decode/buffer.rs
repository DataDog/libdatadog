// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::{BytesData, DeserializableTraceData, SliceData};
use rmp::decode;
use rmp::decode::DecodeStringError;
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

/// Cursor capabilities the msgpack decoder needs on top of rmp's (sealed) [`rmp::decode::RmpRead`]:
/// viewing the unconsumed bytes, and advancing without producing a value.
pub trait RmpCursor: rmp::decode::RmpRead {
    /// Returns the unconsumed bytes.
    fn remaining(&self) -> &[u8];

    /// Advances the cursor by `bytes` without producing a value. Returns `false` (and leaves
    /// the cursor untouched) if fewer than `bytes` bytes remain.
    fn advance(&mut self, bytes: usize) -> bool;
}

impl RmpCursor for decode::Bytes<'_> {
    #[inline]
    fn remaining(&self) -> &[u8] {
        decode::Bytes::remaining_slice(self)
    }

    #[inline]
    fn advance(&mut self, bytes: usize) -> bool {
        let remaining = decode::Bytes::remaining_slice(self);
        if bytes > remaining.len() {
            return false;
        }
        // `rmp::decode::Bytes` has no public in-place advance, so rebuild the cursor over the
        // remaining bytes: O(1), no data movement, no allocation. Its position (only used for
        // rmp error reporting, which the decoder discards) resets to zero.
        *self = decode::Bytes::new(&remaining[bytes..]);
        true
    }
}

/// Internal Buffer used to wrap msgpack data for decoding.
/// Provides a couple accessors to extract data from the buffer.
///
/// The buffer **borrows** the payload for `'a` (the caller must keep it alive for as long as
/// the buffer is used) and tracks the read position in a [`rmp::decode::Bytes`] cursor over the
/// payload, at the payload's honest lifetime. No `'static` lie, no unsafe.
pub struct Buffer<'a, T: DeserializableTraceData> {
    /// Owning handle over the **full** payload. It never advances: it exists so values can be
    /// interned against it (`BytesString` / refcounted slicing for `BytesData`, the borrowed
    /// `&'a [u8]` itself for `SliceData`).
    owner: T::Bytes,
    /// `rmp` read cursor over the unconsumed portion of the payload. This is where all decoding
    /// position lives: `rmp::decode` functions advance it in place through the mutable handle
    /// exposed by [`Buffer::as_mut_slice`].
    cursor: T::Cursor<'a>,
}

/// Decoding buffer over a borrowed slice: the payload and the cursor share the slice's real
/// lifetime `'a`.
impl<'a> Buffer<'a, SliceData<'a>> {
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Buffer {
            owner: data,
            cursor: decode::Bytes::new(data),
        }
    }
}

/// Decoding buffer over a refcounted `Bytes` payload: the cursor borrows the caller's `Bytes`
/// for `'a`, and the buffer keeps its own handle so interning can clone the refcount.
impl<'a> Buffer<'a, BytesData> {
    #[must_use]
    pub fn new(data: &'a libdd_tinybytes::Bytes) -> Self {
        Buffer {
            // A refcount bump (the caller keeps the allocation alive for `'a` anyway).
            owner: data.clone(),
            cursor: decode::Bytes::new(data.as_ref()),
        }
    }
}

impl<'a, T: DeserializableTraceData> Buffer<'a, T> {
    /// Returns a mutable handle to the buffer's `rmp` read cursor over its unconsumed bytes.
    /// `rmp::decode` functions take such a handle (`&mut impl RmpRead`) and advance it, which
    /// advances the buffer.
    pub fn as_mut_slice(&mut self) -> &mut T::Cursor<'a> {
        &mut self.cursor
    }

    /// Returns an immutable reference to the remaining bytes, without advancing the buffer.
    pub fn as_slice(&self) -> &[u8] {
        self.cursor.remaining()
    }

    /// Returns the owning handle over the buffer's full payload.
    pub fn bytes(&self) -> &T::Bytes {
        &self.owner
    }

    /// Advances the cursor by `bytes` without producing a value. Returns `false` (and leaves
    /// the cursor untouched) if fewer than `bytes` bytes remain.
    pub fn advance(&mut self, bytes: usize) -> bool {
        self.cursor.advance(bytes)
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

impl<T: DeserializableTraceData> Deref for Buffer<'_, T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.cursor.remaining()
    }
}
