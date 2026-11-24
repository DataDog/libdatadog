// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::DeserializableTraceData;
use rmp::Marker;
use rmp::decode;
use rmp::decode::DecodeStringError;

use std::borrow::Borrow;
use std::ops::Deref;

/// Cold error path for `read_str_bytes_raw` and `read_string_ref_nomut` to keep the hot path
/// small and easily inlinable.
#[cold]
#[inline(never)]
fn read_str_error(buf: &[u8]) -> DecodeError {
    match decode::read_str_from_slice(buf) {
        Err(DecodeStringError::InvalidMarkerRead(e)) => DecodeError::InvalidFormat(e.to_string()),
        Err(DecodeStringError::InvalidDataRead(e)) => DecodeError::InvalidConversion(e.to_string()),
        Err(DecodeStringError::TypeMismatch(marker)) => {
            DecodeError::InvalidType(format!("Type mismatch at marker {marker:?}"))
        }
        Err(DecodeStringError::InvalidUtf8(_, e)) => DecodeError::Utf8Error(e.to_string()),
        _ => DecodeError::IOError,
    }
}

/// Read the raw bytes of a msgpack string from `buf` without UTF-8 validation.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid msgpack string header and payload.
#[inline(always)]
pub(crate) fn read_str_bytes_raw(buf: &[u8]) -> Result<(&[u8], &[u8]), DecodeError> {
    let (&marker, rest) = buf.split_first().ok_or_else(|| read_str_error(buf))?;
    let (len, data) = if (marker & 0xe0) == 0xa0 {
        // FixStr (0xa0..=0xbf): 0..=31 bytes, by far the most common case in APM traces.
        ((marker & 0x1f) as usize, rest)
    } else if marker == 0xd9 {
        // Str8
        let (&len, data) = rest.split_first().ok_or_else(|| read_str_error(buf))?;
        (len as usize, data)
    } else if marker == 0xda {
        // Str16
        if rest.len() < 2 {
            return Err(read_str_error(buf));
        }
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        (len, &rest[2..])
    } else if marker == 0xdb {
        // Str32
        if rest.len() < 4 {
            return Err(read_str_error(buf));
        }
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        (len, &rest[4..])
    } else {
        return Err(DecodeError::InvalidType(format!(
            "Type mismatch at marker {:?}",
            Marker::from_u8(marker)
        )));
    };

    if len > data.len() {
        return Err(read_str_error(buf));
    }
    // SAFETY: `len <= data.len()` was just checked.
    let (str_bytes, remaining) = unsafe { (data.get_unchecked(..len), data.get_unchecked(len..)) };
    Ok((str_bytes, remaining))
}

#[cold]
#[inline(never)]
fn validate_non_ascii_utf8(bytes: &[u8]) -> Result<&str, DecodeError> {
    std::str::from_utf8(bytes).map_err(|e| DecodeError::Utf8Error(e.to_string()))
}

/// Fast ASCII check using unaligned 8-byte SWAR loads, avoiding `align_to` overhead on the
/// short (3..32 byte) strings that dominate APM traces.
#[inline(always)]
fn is_ascii_fast(bytes: &[u8]) -> bool {
    let (chunks, remainder) = bytes.as_chunks::<8>();
    for &chunk in chunks {
        if (u64::from_ne_bytes(chunk) & 0x8080_8080_8080_8080) != 0 {
            return false;
        }
    }
    let mut or_acc = 0u8;
    for &b in remainder {
        or_acc |= b;
    }
    (or_acc & 0x80) == 0
}

/// Read a string from `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string.
#[inline(always)]
pub fn read_string_ref_nomut(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
    let (str_bytes, remaining) = read_str_bytes_raw(buf)?;
    if is_ascii_fast(str_bytes) {
        // SAFETY: Every ASCII byte sequence is valid UTF-8.
        let s = unsafe { std::str::from_utf8_unchecked(str_bytes) };
        Ok((s, remaining))
    } else {
        let s = validate_non_ascii_utf8(str_bytes)?;
        Ok((s, remaining))
    }
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

    /// Returns the underlying owned bytes buffer.
    pub fn bytes(&self) -> &T::Bytes {
        &self.0
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
