// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::number::read_number_bytes;
use crate::msgpack_decoder::decode::string::{handle_null_marker, read_string_bytes};
use std::collections::HashMap;
use tinybytes::{Bytes, BytesString};

#[inline]
pub fn read_metric_pair(buf: &mut Bytes) -> Result<(BytesString, f64), DecodeError> {
    let key = read_string_bytes(buf)?;
    let v = read_number_bytes(buf)?;

    Ok((key, v))
}
#[inline]
pub fn read_metrics(buf: &mut Bytes) -> Result<HashMap<BytesString, f64>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;

    read_map(len, buf, read_metric_pair)
}
