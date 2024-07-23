// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::v04::decoder::{read_map_strs, read_string_ref};
use crate::msgpack_decoder::v04::error::DecodeError;
use crate::msgpack_decoder::v04::number::read_number;
use datadog_trace_protobuf::pb::SpanLink;
use rmp::Marker;
use std::str::FromStr;

pub(crate) fn read_span_links(buf: &mut &[u8]) -> Result<Vec<SpanLink>, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| DecodeError::WrongFormat)? {
        Marker::FixArray(len) => {
            let mut vec: Vec<SpanLink> = Vec::new();
            for _ in 0..len {
                vec.push(decode_span_link(buf)?);
            }
            Ok(vec)
        }
        _ => Err(DecodeError::WrongType),
    }
}
enum SpanLinkKey {
    TraceId,
    TraceIdHigh,
    SpanId,
    Attributes,
    Tracestate,
    Flags,
}
impl FromStr for SpanLinkKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "trace_id" => Ok(SpanLinkKey::TraceId),
            "trace_id_high" => Ok(SpanLinkKey::TraceIdHigh),
            "span_id" => Ok(SpanLinkKey::SpanId),
            "attributes" => Ok(SpanLinkKey::Attributes),
            "tracestate" => Ok(SpanLinkKey::Tracestate),
            "flags" => Ok(SpanLinkKey::Flags),
            _ => Err(DecodeError::WrongFormat),
        }
    }
}

fn decode_span_link(buf: &mut &[u8]) -> Result<SpanLink, DecodeError> {
    let mut span = SpanLink::default();
    let span_size = rmp::decode::read_map_len(buf).map_err(|_| DecodeError::WrongType)?;

    for _ in 0..span_size {
        let (key, value) = read_string_ref(buf)?;
        *buf = value;
        let key = key.parse::<SpanLinkKey>()?;

        match key {
            SpanLinkKey::TraceId => span.trace_id = read_number(buf)?.try_into()?,
            SpanLinkKey::TraceIdHigh => span.trace_id_high = read_number(buf)?.try_into()?,
            SpanLinkKey::SpanId => span.span_id = read_number(buf)?.try_into()?,
            SpanLinkKey::Attributes => span.attributes = read_map_strs(buf)?,
            SpanLinkKey::Tracestate => {
                let (value, next) = read_string_ref(buf)?;
                span.tracestate = String::from_str(value).unwrap();
                *buf = next;
            }
            SpanLinkKey::Flags => span.flags = read_number(buf)?.try_into()?,
        }
    }

    Ok(span)
}
