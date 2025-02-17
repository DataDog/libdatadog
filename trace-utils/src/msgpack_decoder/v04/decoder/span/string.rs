// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::is_null_marker;
use crate::msgpack_decoder::v04::error::DecodeError;
use rmp::decode::{self, DecodeStringError};

#[inline]
fn read_string_nomut(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
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

/// Read a string from `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string.
#[inline]
pub fn read_string<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    read_string_nomut(buf).map(|(str, newbuf)| {
        *buf = newbuf;
        str
    })
}

/// Read a nullable string from `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string or a null marker.
#[inline]
pub fn read_nullable_string<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    if is_null_marker(buf) {
        Ok("")
    } else {
        read_string(buf)
    }
}
