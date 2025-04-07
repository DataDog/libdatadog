// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::string::{is_null_marker, read_string_ref};
use rmp::decode;
use std::collections::HashMap;
use tinybytes::Bytes;

fn read_byte_array_len(buf: &mut &[u8]) -> Result<u32, DecodeError> {
    decode::read_bin_len(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read binary len for meta_struct".to_owned())
    })
}

#[inline]
pub fn read_meta_struct<'a>(buf: &mut &'a [u8]) -> Result<HashMap<&'a str, Bytes>, DecodeError> {
    if is_null_marker(buf) {
        return Ok(HashMap::default());
    }

    fn read_meta_struct_pair<'a>(buf: &mut &'a [u8]) -> Result<(&'a str, Bytes), DecodeError> {
        let key = read_string_ref(buf)?;
        let byte_array_len = read_byte_array_len(buf)? as usize;

        let slice = buf.get(0..byte_array_len);
        if let Some(slice) = slice {
            let data = Bytes::copy_from_slice(slice);
            *buf = &buf[byte_array_len..];
            Ok((key, data))
        } else {
            Err(DecodeError::InvalidFormat(
                "Invalid data length".to_string(),
            ))
        }
    }

    let len = read_map_len(buf)?;
    read_map(len, buf, read_meta_struct_pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_meta_test() {
        let meta = HashMap::from([("key".to_string(), Bytes::from(vec![1, 2, 3, 4]))]);

        let serialized = rmp_serde::to_vec_named(&meta).unwrap();
        let mut slice =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(serialized.as_ref()) };
        let res = read_meta_struct(&mut slice).unwrap();

        assert_eq!(res.get("key").unwrap().to_vec(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_meta_wrong_family_test() {
        let meta = HashMap::from([("key".to_string(), vec![1, 2, 3, 4])]);

        let serialized = rmp_serde::to_vec_named(&meta).unwrap();
        let mut slice =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(serialized.as_ref()) };
        let res = read_meta_struct(&mut slice);

        assert!(res.is_err());
        matches!(res.unwrap_err(), DecodeError::InvalidFormat(_));
    }
}
