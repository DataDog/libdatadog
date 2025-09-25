// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::map::{read_map, read_map_len};
use crate::msgpack_decoder::decode::number::read_number;
use crate::msgpack_decoder::decode::string::handle_null_marker;
use crate::span::TraceData;
use std::collections::HashMap;

#[inline]
pub fn read_metric_pair<T: TraceData>(buf: &mut Buffer<T>) -> Result<(T::Text, f64), DecodeError> {
    let key = buf.read_string()?;
    let v = read_number(buf)?;

    Ok((key, v))
}
#[inline]
pub fn read_metrics<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<HashMap<T::Text, f64>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(HashMap::default());
    }

    let len = read_map_len(buf)?;

    read_map(len, buf, read_metric_pair)
}
