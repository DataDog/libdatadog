// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::string::handle_null_marker;
use crate::span::TraceData;
use rmp::decode;
use std::collections::HashMap;

fn read_byte_array_len<T: TraceData>(buf: &mut Buffer<T>) -> Result<u32, DecodeError> {
    decode::read_bin_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read binary len for meta_struct".to_owned())
    })
}

#[inline]
pub fn read_meta_struct<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<HashMap<T::Text, T::Bytes>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(HashMap::default());
    }

    fn read_meta_struct_pair<T: TraceData>(
        buf: &mut Buffer<T>,
    ) -> Result<(T::Text, T::Bytes), DecodeError> {
        let key = buf.read_string()?;
        let byte_array_len = read_byte_array_len(buf)? as usize;

        if let Some(data) = buf.try_slice_and_advance(byte_array_len) {
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
    use crate::span::SliceData;
    use libdd_tinybytes::Bytes;

    #[test]
    fn read_meta_test() {
        let meta = HashMap::from([("key".to_string(), Bytes::from(vec![1, 2, 3, 4]))]);

        let serialized = rmp_serde::to_vec_named(&meta).unwrap();
        let mut slice = Buffer::<SliceData>::new(serialized.as_ref());
        let res = read_meta_struct(&mut slice).unwrap();

        assert_eq!(res.get("key").unwrap().to_vec(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn read_meta_wrong_family_test() {
        let meta = HashMap::from([("key".to_string(), vec![1, 2, 3, 4])]);

        let serialized = rmp_serde::to_vec_named(&meta).unwrap();
        let mut slice = Buffer::<SliceData>::new(serialized.as_ref());
        let res = read_meta_struct(&mut slice);

        assert!(res.is_err());
        matches!(res.unwrap_err(), DecodeError::InvalidFormat(_));
    }
}
