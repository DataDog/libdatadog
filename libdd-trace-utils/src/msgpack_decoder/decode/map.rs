// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::{buffer::Buffer, error::DecodeError};
use crate::span::DeserializableTraceData;
use crate::span::vec_map::VecMap;
use std::collections::HashMap;

/// Reads a map from the buffer and returns it as a `HashMap`.
///
/// This function is generic over the key and value types of the map, and it uses a provided
/// function to read key-value pairs from the buffer.
///
/// # Arguments
///
/// * `len` - The number of key-value pairs to read from the buffer.
/// * `buf` - A reference to the slice containing the encoded map data.
/// * `read_pair` - A function that reads a key-value pair from the buffer and returns it as a
///   `Result<(K, V), DecodeError>`.
///
/// # Returns
///
/// * `Ok(HashMap<K, V>)` - A `HashMap` containing the decoded key-value pairs if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The `read_pair` function returns an error while reading a key-value pair.
///
/// # Type Parameters
///
/// * `K` - The type of the keys in the map. Must implement `std::hash::Hash` and `Eq`.
/// * `V` - The type of the values in the map.
/// * `F` - The type of the function used to read key-value pairs from the buffer.
#[inline]
pub fn read_map<K, V, F, B>(
    len: usize,
    buf: &mut B,
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut B) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Reads a map from the buffer and returns it as a `VecMap<K, V>`.
///
/// Like `read_map` but returns pairs in a VecMap instead of HashMap for better
/// cache locality when iteration order doesn't matter.
#[inline]
pub fn read_map_vec<K, V, F, B>(
    len: usize,
    buf: &mut B,
    read_pair: F,
) -> Result<VecMap<K, V>, DecodeError>
where
    F: Fn(&mut B) -> Result<(K, V), DecodeError>,
{
    let mut map = VecMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Reads map length from the buffer
///
/// # Arguments
///
/// * `buf` - A reference to the Bytes containing the encoded map data.
///
/// # Returns
///
/// * `Ok(usize)` - Map length.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The buffer does not contain a map.
/// - There is an error reading from the buffer.
#[inline(always)]
pub fn read_map_len<T: DeserializableTraceData>(buf: &mut Buffer<T>) -> Result<usize, DecodeError> {
    let slice = buf.as_mut_slice();
    let (&marker, rest) = slice
        .split_first()
        .ok_or_else(|| DecodeError::InvalidFormat("Unable to read marker for map".to_owned()))?;
    if (marker & 0xf0) == 0x80 {
        *slice = rest;
        Ok((marker & 0x0f) as usize)
    } else if marker == 0xde {
        if rest.len() < 2 {
            return Err(DecodeError::IOError);
        }
        let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
        *slice = &rest[2..];
        Ok(len)
    } else if marker == 0xdf {
        if rest.len() < 4 {
            return Err(DecodeError::IOError);
        }
        let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        *slice = &rest[4..];
        Ok(len)
    } else {
        Err(DecodeError::InvalidType(
            "Unable to read map from buffer".to_owned(),
        ))
    }
}
