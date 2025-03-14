// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::string::{handle_null_marker, read_string_bytes};
use rmp::decode;
use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

fn read_byte_array_len(buf: &mut &[u8]) -> Result<u32, DecodeError> {
    decode::read_bin_len(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read binary len for meta_struct".to_owned())
    })
}

#[inline]
pub fn read_meta_struct(buf: &mut Bytes) -> Result<HashMap<BytesString, Bytes>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    fn read_meta_struct_pair(buf: &mut Bytes) -> Result<(BytesString, Bytes), DecodeError> {
        let key = read_string_bytes(buf)?;
        let byte_array_len = read_byte_array_len(unsafe { buf.as_mut_slice() })? as usize;

        let data = buf.slice_ref(&buf[0..byte_array_len]).unwrap();
        unsafe {
            // SAFETY: forwarding the buffer requires that buf is borrowed from static.
            *buf.as_mut_slice() = &buf.as_mut_slice()[byte_array_len..];
        }

        Ok((key, data))
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;
    read_map(len, buf, read_meta_struct_pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_meta_test() {
        let meta = HashMap::from([("key".to_string(), Bytes::from(vec![1, 2, 3, 4]))]);

        let mut bytes = Bytes::from(rmp_serde::to_vec_named(&meta).unwrap());
        let res = read_meta_struct(&mut bytes).unwrap();

        assert_eq!(res.get("key").unwrap().to_vec(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_meta_wrong_family_test() {
        let meta = HashMap::from([("key".to_string(), vec![1, 2, 3, 4])]);

        let mut bytes = Bytes::from(rmp_serde::to_vec_named(&meta).unwrap());
        let res = read_meta_struct(&mut bytes);

        assert!(res.is_err());
        matches!(res.unwrap_err(), DecodeError::InvalidFormat(_));
    }
}
