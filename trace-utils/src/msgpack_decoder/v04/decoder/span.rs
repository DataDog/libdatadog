// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    read_meta_struct, read_metrics, read_str_map_to_bytes_strings, read_string_bytes,
    read_string_ref, span_link::read_span_links,
};
use crate::msgpack_decoder::v04::error::DecodeError;
use crate::msgpack_decoder::v04::number::read_number_bytes;
use crate::span_v04::{Span, SpanKey};
use tinybytes::Bytes;

/// Decodes a slice of bytes into a `Span` object.
///
/// # Arguments
///
/// * `buf` - A mutable reference to a slice of bytes containing the encoded data.
///
/// # Returns
///
/// * `Ok(Span)` - A decoded `Span` object if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The map length cannot be read.
/// - Any key or value cannot be decoded.
pub fn decode_span(buffer: &mut Bytes) -> Result<Span, DecodeError> {
    let mut span = Span::default();

    let span_size = rmp::decode::read_map_len(unsafe { buffer.as_mut_slice() }).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(&mut span, buffer)?;
    }

    Ok(span)
}

// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings
fn fill_span(span: &mut Span, buf: &mut Bytes) -> Result<(), DecodeError> {
    let key = read_string_ref(unsafe { buf.as_mut_slice() })?
        .parse::<SpanKey>()
        .map_err(|_| DecodeError::InvalidFormat("Invalid span key".to_owned()))?;

    match key {
        SpanKey::Service => span.service = read_string_bytes(buf)?,
        SpanKey::Name => span.name = read_string_bytes(buf)?,
        SpanKey::Resource => span.resource = read_string_bytes(buf)?,
        SpanKey::TraceId => span.trace_id = read_number_bytes(buf)?,
        SpanKey::SpanId => span.span_id = read_number_bytes(buf)?,
        SpanKey::ParentId => span.parent_id = read_number_bytes(buf)?,
        SpanKey::Start => span.start = read_number_bytes(buf)?,
        SpanKey::Duration => span.duration = read_number_bytes(buf)?,
        SpanKey::Error => span.error = read_number_bytes(buf)?,
        SpanKey::Type => span.r#type = read_string_bytes(buf)?,
        SpanKey::Meta => span.meta = read_str_map_to_bytes_strings(buf)?,
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
