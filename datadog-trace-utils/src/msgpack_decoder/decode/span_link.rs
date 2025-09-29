// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_number;
use crate::msgpack_decoder::decode::string::{handle_null_marker, read_str_map_to_strings};
use crate::span::v04::SpanLink;
use crate::span::TraceData;
use std::borrow::Borrow;
use std::str::FromStr;

/// Reads a slice of bytes and decodes it into a vector of `SpanLink` objects.
///
/// # Arguments
///
/// * `buf` - A mutable reference to a slice of bytes containing the encoded data.
///
/// # Returns
///
/// * `Ok(Vec<SpanLink>)` - A vector of decoded `SpanLink` objects if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The marker for the array length cannot be read.
/// - Any `SpanLink` cannot be decoded.
/// ```
pub(crate) fn read_span_links<T: TraceData>(
    buf: &mut Buffer<T>,
) -> Result<Vec<SpanLink<T>>, DecodeError> {
    if handle_null_marker(buf) {
        return Ok(Vec::default());
    }

    let len = rmp::decode::read_array_len(buf.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidType("Unable to get array len for span links".to_owned())
    })?;

    let mut vec: Vec<SpanLink<T>> = Vec::with_capacity(len as usize);
    for _ in 0..len {
        vec.push(decode_span_link(buf)?);
    }
    Ok(vec)
}
#[derive(Debug, PartialEq)]
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
            _ => Err(DecodeError::InvalidFormat(
                format!("Invalid span link key: {s}").to_owned(),
            )),
        }
    }
}

fn decode_span_link<T: TraceData>(buf: &mut Buffer<T>) -> Result<SpanLink<T>, DecodeError> {
    let mut span = SpanLink::default();
    let span_size = rmp::decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for span size".to_owned()))?;

    for _ in 0..span_size {
        match buf.read_string()?.borrow().parse::<SpanLinkKey>()? {
            SpanLinkKey::TraceId => span.trace_id = read_number(buf)?,
            SpanLinkKey::TraceIdHigh => span.trace_id_high = read_number(buf)?,
            SpanLinkKey::SpanId => span.span_id = read_number(buf)?,
            SpanLinkKey::Attributes => span.attributes = read_str_map_to_strings(buf)?,
            SpanLinkKey::Tracestate => span.tracestate = buf.read_string()?,
            SpanLinkKey::Flags => span.flags = read_number(buf)?,
        }
    }

    Ok(span)
}

#[cfg(test)]
mod tests {
    use super::SpanLinkKey;
    use crate::msgpack_decoder::decode::error::DecodeError;
    use std::str::FromStr;

    #[test]
    fn test_span_link_key_from_str() {
        // Valid cases
        assert_eq!(
            SpanLinkKey::from_str("trace_id").unwrap(),
            SpanLinkKey::TraceId
        );
        assert_eq!(
            SpanLinkKey::from_str("trace_id_high").unwrap(),
            SpanLinkKey::TraceIdHigh
        );
        assert_eq!(
            SpanLinkKey::from_str("span_id").unwrap(),
            SpanLinkKey::SpanId
        );
        assert_eq!(
            SpanLinkKey::from_str("attributes").unwrap(),
            SpanLinkKey::Attributes
        );
        assert_eq!(
            SpanLinkKey::from_str("tracestate").unwrap(),
            SpanLinkKey::Tracestate
        );
        assert_eq!(SpanLinkKey::from_str("flags").unwrap(), SpanLinkKey::Flags);

        // Invalid case
        assert!(matches!(
            SpanLinkKey::from_str("invalid_key"),
            Err(DecodeError::InvalidFormat(_))
        ));
    }
}
