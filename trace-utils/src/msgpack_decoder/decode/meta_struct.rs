// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::number::read_number_bytes;
use crate::msgpack_decoder::decode::string::{handle_null_marker, read_string_bytes};
use rmp::decode;
use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

pub fn read_meta_struct(buf: &mut Bytes) -> Result<HashMap<BytesString, Vec<u8>>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    fn read_meta_struct_pair(buf: &mut Bytes) -> Result<(BytesString, Vec<u8>), DecodeError> {
        let key = read_string_bytes(buf)?;
        let array_len = decode::read_array_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;

        let mut v = Vec::with_capacity(array_len as usize);

        for _ in 0..array_len {
            let value = read_number_bytes(buf)?;
            v.push(value);
        }
        Ok((key, v))
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;
    read_map(len, buf, read_meta_struct_pair)
}
