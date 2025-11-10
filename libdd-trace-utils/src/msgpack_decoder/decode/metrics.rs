// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::number::read_number_slice;
use crate::msgpack_decoder::decode::string::{handle_null_marker, read_string_ref};
use std::collections::HashMap;

#[inline]
pub fn read_metric_pair<'a>(buf: &mut &'a [u8]) -> Result<(&'a str, f64), DecodeError> {
    let key = read_string_ref(buf)?;
    let v = read_number_slice(buf)?;

    Ok((key, v))
}
#[inline]
pub fn read_metrics<'a>(buf: &mut &'a [u8]) -> Result<HashMap<&'a str, f64>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(HashMap::default());
    }

    let len = read_map_len(buf)?;

    read_map(len, buf, read_metric_pair)
}
