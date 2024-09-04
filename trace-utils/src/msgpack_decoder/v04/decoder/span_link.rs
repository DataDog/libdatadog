// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::v04::decoder::{read_str_map_to_bytes_strings, read_string_ref};
use crate::msgpack_decoder::v04::error::DecodeError;
use crate::msgpack_decoder::v04::number::read_number;
use crate::span_v04::SpanLink;
use rmp::Marker;
use std::str::FromStr;
use tinybytes::bytes_string::BufferWrapper;

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
pub(crate) fn read_span_links(
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<Vec<SpanLink>, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read marker for span links".to_owned())
    })? {
        Marker::FixArray(len) => {
            let mut vec: Vec<SpanLink> = Vec::with_capacity(len.into());
            for _ in 0..len {
                vec.push(decode_span_link(buf_wrapper, buf)?);
            }
            Ok(vec)
        }
        _ => Err(DecodeError::InvalidType(
            "Unable to read span link from buffer".to_owned(),
        )),
    }
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
                format!("Invalid span link key: {}", s).to_owned(),
            )),
        }
    }
}

fn decode_span_link(buf_wrapper: &BufferWrapper, buf: &mut &[u8]) -> Result<SpanLink, DecodeError> {
    let mut span = SpanLink::default();
    let span_size = rmp::decode::read_map_len(buf)
        .map_err(|_| DecodeError::InvalidType("Unable to get map len for span size".to_owned()))?;

    for _ in 0..span_size {
        let (key, value) = read_string_ref(buf)?;
        *buf = value;
        let key = key.parse::<SpanLinkKey>()?;

        match key {
            SpanLinkKey::TraceId => span.trace_id = read_number(buf)?.try_into()?,
            SpanLinkKey::TraceIdHigh => span.trace_id_high = read_number(buf)?.try_into()?,
            SpanLinkKey::SpanId => span.span_id = read_number(buf)?.try_into()?,
            SpanLinkKey::Attributes => {
                span.attributes = read_str_map_to_bytes_strings(buf_wrapper, buf)?
            }
            SpanLinkKey::Tracestate => {
                let (val, next) = read_string_ref(buf)?;
                span.tracestate =
                    unsafe { buf_wrapper.create_bytes_string_unchecked(val.as_bytes()) };
                *buf = next;
            }
            SpanLinkKey::Flags => span.flags = read_number(buf)?.try_into()?,
        }
    }

    Ok(span)
}

#[cfg(test)]
mod tests {
    use super::SpanLinkKey;
    use crate::msgpack_decoder::v04::error::DecodeError;
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
