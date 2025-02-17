// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Read maps from msgpack
mod map;
/// Read numbers from msgpack
mod number;
/// Read span links from msgpack
mod span_link;
/// Read strings from msgpack
mod string;

use crate::msgpack_decoder::v04::error::DecodeError;
use crate::span_v04::{SpanKey, SpanSlice};
use map::{read_meta_struct, read_metrics, read_nullable_str_map_to_str};
use number::read_nullable_number;
use span_link::read_span_links;
use string::{read_nullable_string, read_string};

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

/// When you want to "peek" if the next value is a null marker, and only advance the buffer if it is
/// null. If it is not null, you can continue to decode as expected.
#[inline]
fn is_null_marker(buf: &mut &[u8]) -> bool {
    if buf.first() == Some(NULL_MARKER) {
        *buf = &buf[1..];
        true
    } else {
        false
    }
}

/// Decodes a slice of bytes into a `SpanSlice` object.
///
/// # Arguments
///
/// * `buf` - A mutable reference to a slice of bytes containing the encoded data.
///
/// # Returns
///
/// * `Ok(Span)` - A decoded `SpanSlice` object if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The map length cannot be read.
/// - Any key or value cannot be decoded.
pub fn decode_span<'a>(buffer: &mut &'a [u8]) -> Result<SpanSlice<'a>, DecodeError> {
    let mut span = SpanSlice::default();

    let span_size = rmp::decode::read_map_len(buffer).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(&mut span, buffer)?;
    }

    Ok(span)
}

/// Read the next entry from `buf` and update `span` corresponding field.
fn fill_span<'a>(span: &mut SpanSlice<'a>, buf: &mut &'a [u8]) -> Result<(), DecodeError> {
    let key = read_string(buf)?
        .parse::<SpanKey>()
        .map_err(|_| DecodeError::InvalidFormat("Invalid span key".to_owned()))?;

    match key {
        SpanKey::Service => span.service = read_nullable_string(buf)?,
        SpanKey::Name => span.name = read_nullable_string(buf)?,
        SpanKey::Resource => span.resource = read_nullable_string(buf)?,
        SpanKey::TraceId => span.trace_id = read_nullable_number(buf)?,
        SpanKey::SpanId => span.span_id = read_nullable_number(buf)?,
        SpanKey::ParentId => span.parent_id = read_nullable_number(buf)?,
        SpanKey::Start => span.start = read_nullable_number(buf)?,
        SpanKey::Duration => span.duration = read_nullable_number(buf)?,
        SpanKey::Error => span.error = read_nullable_number(buf)?,
        SpanKey::Type => span.r#type = read_nullable_string(buf)?,
        SpanKey::Meta => span.meta = read_nullable_str_map_to_str(buf)?,
        SpanKey::Metrics => span.metrics = read_metrics(buf)?,
        SpanKey::MetaStruct => span.meta_struct = read_meta_struct(buf)?,
        SpanKey::SpanLinks => span.span_links = read_span_links(buf)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SpanKey;
    use crate::span_v04::SpanKeyParseError;
    use std::str::FromStr;

    #[test]
    fn test_span_key_from_str() {
        assert_eq!(SpanKey::from_str("service").unwrap(), SpanKey::Service);
        assert_eq!(SpanKey::from_str("name").unwrap(), SpanKey::Name);
        assert_eq!(SpanKey::from_str("resource").unwrap(), SpanKey::Resource);
        assert_eq!(SpanKey::from_str("trace_id").unwrap(), SpanKey::TraceId);
        assert_eq!(SpanKey::from_str("span_id").unwrap(), SpanKey::SpanId);
        assert_eq!(SpanKey::from_str("parent_id").unwrap(), SpanKey::ParentId);
        assert_eq!(SpanKey::from_str("start").unwrap(), SpanKey::Start);
        assert_eq!(SpanKey::from_str("duration").unwrap(), SpanKey::Duration);
        assert_eq!(SpanKey::from_str("error").unwrap(), SpanKey::Error);
        assert_eq!(SpanKey::from_str("meta").unwrap(), SpanKey::Meta);
        assert_eq!(SpanKey::from_str("metrics").unwrap(), SpanKey::Metrics);
        assert_eq!(SpanKey::from_str("type").unwrap(), SpanKey::Type);
        assert_eq!(
            SpanKey::from_str("meta_struct").unwrap(),
            SpanKey::MetaStruct
        );
        assert_eq!(SpanKey::from_str("span_links").unwrap(), SpanKey::SpanLinks);

        let invalid_result = SpanKey::from_str("invalid_key");
        let msg = format!("SpanKeyParseError: Invalid span key: {}", "invalid_key");
        assert!(matches!(invalid_result, Err(SpanKeyParseError { .. })));
        assert_eq!(invalid_result.unwrap_err().to_string(), msg);
    }
}
