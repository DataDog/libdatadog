// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use rmp::{decode, decode::RmpRead, Marker};
use std::collections::HashMap;
use tinybytes::Bytes;

/// Reads a map from the buffer and returns it as a `HashMap`.
///
/// This function is generic over the key and value types of the map, and it uses a provided
/// function to read key-value pairs from the buffer.
///
/// # Arguments
///
/// * `len` - The number of key-value pairs to read from the buffer.
/// * `buf` - A reference to the Bytes containing the encoded map data.
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
pub fn read_map<K, V, F>(
    len: usize,
    buf: &mut Bytes,
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut Bytes) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
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
#[inline]
pub fn read_map_len(buf: &mut &[u8]) -> Result<usize, DecodeError> {
    match decode::read_marker(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to read marker for map".to_owned()))?
    {
        Marker::FixMap(len) => Ok(len as usize),
        Marker::Map16 => buf
            .read_data_u16()
            .map_err(|_| DecodeError::IOError)
            .map(|len| len as usize),
        Marker::Map32 => buf
            .read_data_u32()
            .map_err(|_| DecodeError::IOError)
            .map(|len| len as usize),
        _ => Err(DecodeError::InvalidType(
            "Unable to read map from buffer".to_owned(),
        )),
    }
}
