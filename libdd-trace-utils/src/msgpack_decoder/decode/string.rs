// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::TraceData;
use rmp::decode;
use std::collections::HashMap;

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

/// Read a nullable string from the slices `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string or a null marker.
#[inline]
pub fn read_nullable_string<T: TraceData>(buf: &mut Buffer<T>) -> Result<T::Text, DecodeError> {
    if handle_null_marker(buf) {
        Ok(T::Text::default())
    } else {
        buf.read_string()
    }
}

/// Read a hashmap of (string, string) from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
/// Null values are skipped (key not inserted into map).
#[inline]
pub fn read_str_map_to_strings<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<HashMap<T::Text, T::Text>, DecodeError> {
    let len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    #[allow(clippy::expect_used)]
    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = buf.read_string()?;
        // Only insert if value is not null
        if !handle_null_marker(buf) {
            let value = buf.read_string()?;
            map.insert(key, value);
        }
    }
    Ok(map)
}

/// Read a nullable hashmap of (string, string) from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
/// Null values are skipped (key not inserted into map).
#[inline]
pub fn read_nullable_str_map_to_strings<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<HashMap<T::Text, T::Text>, DecodeError> {
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
pub fn handle_null_marker<T: TraceData>(buf: &mut Buffer<T>) -> bool {
    let slice = buf.as_mut_slice();
    if slice.first() == Some(NULL_MARKER) {
        *slice = &slice[1..];
        true
    } else {
        false
    }
}
