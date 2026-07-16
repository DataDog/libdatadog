// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::DeserializableTraceData;
use rmp::decode;
use rmp::decode::DecodeStringError;

use std::borrow::Borrow;
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

/// Internal Buffer used to wrap msgpack data for decoding.
/// Provides a couple accessors to extract data from the buffer.
pub struct Buffer<T: DeserializableTraceData>(T::Bytes);

impl<T: DeserializableTraceData> Buffer<T> {
    pub fn new(data: T::Bytes) -> Self {
        Buffer(data)
    }

    /// Returns a mutable reference to the underlying slice.
    pub fn as_mut_slice(&mut self) -> &mut &'static [u8] {
        T::get_mut_slice(&mut self.0)
    }

    /// Returns an immutable reference to the underlying slice, without advancing the buffer.
    pub fn as_slice(&self) -> &[u8] {
        self.0.borrow()
    }

    /// Tries to extract a slice of `bytes` from the buffer and advances the buffer.
    pub fn try_slice_and_advance(&mut self, bytes: usize) -> Option<T::Bytes> {
        T::try_slice_and_advance(&mut self.0, bytes)
    }

    /// Read a string from the slices `buf`.
    ///
    /// # Errors
    /// Fails if the buffer doesn't contain a valid utf8 msgpack string.
    pub fn read_string(&mut self) -> Result<T::Text, DecodeError> {
        T::read_string(&mut self.0)
    }

    /// Caps a decoded element count at the bytes remaining in the buffer. Each msgpack
    /// element needs >=1 byte on the wire, so a length prefix can't legitimately exceed
    /// the remaining bytes — this prevents a malicious count (e.g. 0xFFFFFFFF) from
    /// forcing a huge pre-allocation before any element is read.
    pub fn capped_capacity(&self, count: usize) -> usize {
        count.min(self.len())
    }
}

impl<T: DeserializableTraceData> Deref for Buffer<T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.borrow()
    }
}
