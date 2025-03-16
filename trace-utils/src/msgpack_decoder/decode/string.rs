// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use rmp::decode;
use rmp::decode::DecodeStringError;
use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

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

#[inline]
pub fn read_string_ref<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    read_string_ref_nomut(buf).map(|(str, newbuf)| {
        *buf = newbuf;
        str
    })
}

#[inline]
pub fn read_string_bytes(buf: &mut Bytes) -> Result<BytesString, DecodeError> {
    // Note: we need to pass a &'static lifetime here, otherwise it'll complain
    read_string_ref_nomut(unsafe { buf.as_mut_slice() }).map(|(str, newbuf)| {
        let string = BytesString::from_bytes_slice(buf, str);
        *unsafe { buf.as_mut_slice() } = newbuf;
        string
    })
}

#[inline]
pub fn read_nullable_string_bytes(buf: &mut Bytes) -> Result<BytesString, DecodeError> {
    if let Some(empty_string) = handle_null_marker(buf, BytesString::default) {
        Ok(empty_string)
    } else {
        read_string_bytes(buf)
    }
}

#[inline]
// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings.
pub fn read_str_map_to_bytes_strings(
    buf: &mut Bytes,
) -> Result<HashMap<BytesString, BytesString>, DecodeError> {
    let len = decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    #[allow(clippy::expect_used)]
    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = read_string_bytes(buf)?;
        let value = read_string_bytes(buf)?;
        map.insert(key, value);
    }
    Ok(map)
}

#[inline]
pub fn read_nullable_str_map_to_bytes_strings(
    buf: &mut Bytes,
) -> Result<HashMap<BytesString, BytesString>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    read_str_map_to_bytes_strings(buf)
}

/// When you want to "peek" if the next value is a null marker, and only advance the buffer if it is
/// null and return the default value. If it is not null, you can continue to decode as expected.
#[inline]
pub fn handle_null_marker<T, F>(buf: &mut Bytes, default: F) -> Option<T>
where
    F: FnOnce() -> T,
{
    let slice = unsafe { buf.as_mut_slice() };

    if slice.first() == Some(NULL_MARKER) {
        *slice = &slice[1..];
        Some(default())
    } else {
        None
    }
}
