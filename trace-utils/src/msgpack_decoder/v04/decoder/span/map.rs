// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::is_null_marker;
use super::number::read_number;
use super::string::read_string;
use crate::msgpack_decoder::v04::error::DecodeError;
use rmp::{decode, decode::RmpRead, Marker};
use std::collections::HashMap;

/// Read a map of string to string from `buf`.
#[inline]
pub fn read_str_map_to_str<'a>(
    buf: &mut &'a [u8],
) -> Result<HashMap<&'a str, &'a str>, DecodeError> {
    let len = decode::read_map_len(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = read_string(buf)?;
        let value = read_string(buf)?;
        map.insert(key, value);
    }
    Ok(map)
}

/// Read a nullable map of string to string from `buf`.
#[inline]
pub fn read_nullable_str_map_to_str<'a>(
    buf: &mut &'a [u8],
) -> Result<HashMap<&'a str, &'a str>, DecodeError> {
    if is_null_marker(buf) {
        return Ok(HashMap::default());
    }

    read_str_map_to_str(buf)
}

/// Read a map of string to f64 from `buf`.
#[inline]
pub fn read_metrics<'a>(buf: &mut &'a [u8]) -> Result<HashMap<&'a str, f64>, DecodeError> {
    if is_null_marker(buf) {
        return Ok(HashMap::default());
    }

    fn read_metric_pair<'a>(buf: &mut &'a [u8]) -> Result<(&'a str, f64), DecodeError> {
        let key = read_string(buf)?;
        let v = read_number(buf)?;

        Ok((key, v))
    }

    let len = read_map_len(buf)?;

    read_map(len, buf, read_metric_pair)
}

/// Read a map of string to u8 array from `buf`.
///
/// The struct can't be a u8 slice since it is encoded as a msgpack array and not as a raw bytes
/// buffer.
#[inline]
pub fn read_meta_struct<'a>(buf: &mut &'a [u8]) -> Result<HashMap<&'a str, Vec<u8>>, DecodeError> {
    if is_null_marker(buf) {
        return Ok(HashMap::default());
    }

    fn read_meta_struct_pair<'a>(buf: &mut &'a [u8]) -> Result<(&'a str, Vec<u8>), DecodeError> {
        let key = read_string(buf)?;
        let array_len = decode::read_array_len(buf).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;

        let mut v = Vec::with_capacity(array_len as usize);

        for _ in 0..array_len {
            let value = read_number(buf)?;
            v.push(value);
        }
        Ok((key, v))
    }

    let len = read_map_len(buf)?;
    read_map(len, buf, read_meta_struct_pair)
}

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
fn read_map<'a, K, V, F>(
    len: usize,
    buf: &mut &'a [u8],
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut &'a [u8]) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

/// Read the length of a msgpack map.
#[inline]
fn read_map_len(buf: &mut &[u8]) -> Result<usize, DecodeError> {
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
