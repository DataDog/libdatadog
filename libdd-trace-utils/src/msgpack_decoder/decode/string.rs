// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::DeserializableTraceData;
use rmp::decode;

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

/// Read a nullable string from the slices `buf`.
///
/// # Errors
/// Fails if the buffer doesn't contain a valid utf8 msgpack string or a null marker.
#[inline]
pub fn read_nullable_string<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<T::Text, DecodeError> {
    if handle_null_marker(buf) {
        Ok(T::Text::default())
    } else {
        buf.read_string()
    }
}

/// Read a vec of (string, string) pairs from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
/// Null values are skipped (key not inserted into vec).
#[inline]
pub fn read_str_map_to_strings<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<Vec<(T::Text, T::Text)>, DecodeError> {
    let len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    #[allow(clippy::expect_used)]
    let capacity: usize = len.try_into().expect("Unable to cast map len to usize");
    let mut vec = Vec::with_capacity(capacity);
    for _ in 0..len {
        let key = buf.read_string()?;
        // Only insert if value is not null
        if !handle_null_marker(buf) {
            let value = buf.read_string()?;
            vec.push((key, value));
        }
    }
    Ok(vec)
}

/// Read a nullable vec of (string, string) pairs from the slices `buf`.
///
/// # Errors
/// Fails if the buffer does not contain a valid map length prefix,
/// or if any key or value is not a valid utf8 msgpack string.
/// Null values are skipped (key not inserted into vec).
#[inline]
pub fn read_nullable_str_map_to_strings<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<Vec<(T::Text, T::Text)>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(Vec::new());
    }

    read_str_map_to_strings(buf)
}

/// Read a hashmap of (string, string) from the slices `buf`.
/// Used for SpanLink/SpanEvent attributes which remain as HashMap.
#[inline]
pub fn read_str_map_to_hashmap<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
) -> Result<std::collections::HashMap<T::Text, T::Text>, DecodeError>
where
    T::Text: std::hash::Hash + Eq,
{
    let len = decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    #[allow(clippy::expect_used)]
    let capacity: usize = len.try_into().expect("Unable to cast map len to usize");
    let mut map = std::collections::HashMap::with_capacity(capacity);
    for _ in 0..len {
        let key = buf.read_string()?;
        if !handle_null_marker(buf) {
            let value = buf.read_string()?;
            map.insert(key, value);
        }
    }
    Ok(map)
}

/// Handle the null value by peeking if the next value is a null marker, and will only advance the
/// buffer if it is null. If it is not null, you can continue to decode as expected.
///
/// # Returns
/// A boolean indicating whether the next value is null or not.
#[inline]
pub fn handle_null_marker<T: DeserializableTraceData>(buf: &mut Buffer<T>) -> bool {
    let slice = buf.as_mut_slice();
    if slice.first() == Some(NULL_MARKER) {
        *slice = &slice[1..];
        true
    } else {
        false
    }
}
