// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use hashbrown::HashMap;
use rmp::decode;
use rmp::decode::DecodeStringError;

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

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
            DecodeError::InvalidType(format!("Type mismatch at marker {:?}", marker))
        }
        DecodeStringError::InvalidUtf8(_, e) => DecodeError::Utf8Error(e.to_string()),
        _ => DecodeError::IOError,
    })
}

/// Read a string from the slices `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string.
#[inline]
pub fn read_string_ref<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    read_string_ref_nomut(buf).map(|(str, newbuf)| {
        *buf = newbuf;
        str
    })
}

/// Read a nullable string from the slices `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string or a null marker.
#[inline]
pub fn read_nullable_string<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    if handle_null_marker(buf) {
        Ok("")
    } else {
        read_string_ref(buf)
    }
}

/// Read a hashmap of (string, string) from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
#[inline]
pub fn read_str_map_to_strings<'a>(
    buf: &mut &'a [u8],
) -> Result<HashMap<&'a str, &'a str>, DecodeError> {
    let len = decode::read_map_len(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    #[allow(clippy::expect_used)]
    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = read_string_ref(buf)?;
        let value = read_string_ref(buf)?;
        map.insert(key, value);
    }
    Ok(map)
}

/// Read a nullable hashmap of (string, string) from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
#[inline]
pub fn read_nullable_str_map_to_strings<'a>(
    buf: &mut &'a [u8],
) -> Result<HashMap<&'a str, &'a str>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(HashMap::default());
    }

    read_str_map_to_strings(buf)
}

/// Handle the null value by peeking if the next value is a null marker, and will only advance the
/// buffer if it is null. If it is not null, you can continue to decode as expected.
///
/// # Returns
/// A boolean indicating whether the next value is null or not.
#[inline]
pub fn handle_null_marker(buf: &mut &[u8]) -> bool {
    if buf.first() == Some(NULL_MARKER) {
        *buf = &buf[1..];
        true
    } else {
        false
    }
}
