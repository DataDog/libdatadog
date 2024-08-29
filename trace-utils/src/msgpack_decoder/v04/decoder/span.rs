// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    read_str_map_to_no_alloc_strings, read_meta_struct, read_metrics, read_string_ref,
    span_link::read_span_links,
};
use crate::msgpack_decoder::v04::error::DecodeError;
use crate::msgpack_decoder::v04::number::read_number;
use std::str::FromStr;
use crate::no_alloc_string::BufferWrapper;
use crate::span_v04::{Span, SpanKey};

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
pub fn decode_span<'a>(buffer: &'a tinybytes::Bytes, buf: &mut &'a [u8]) -> Result<Span, DecodeError> {
    let mut span = Span::default();
    let wrapper = BufferWrapper::new(buffer.clone()); // Use the Bytes instance directly

    let span_size = rmp::decode::read_map_len(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(&mut span, &wrapper, buf)?;
    }

    Ok(span)
}

fn fill_span(
    span: &mut Span,
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<(), DecodeError> {
    let (key, value) = read_string_ref(buf)?;
    let key = key
        .parse::<SpanKey>()
        .map_err(|_| DecodeError::InvalidFormat("Invalid span key".to_owned()))?;

    *buf = value;

    match key {
        SpanKey::Service => {
            let (value, next) = read_string_ref(buf)?;
            span.service = buf_wrapper.create_no_alloc_string(value.as_bytes());
            *buf = next;
        }
        SpanKey::Name => {
            let (value, next) = read_string_ref(buf)?;
            span.name = buf_wrapper.create_no_alloc_string(value.as_bytes());
            *buf = next;
        }
        SpanKey::Resource => {
            let (value, next) = read_string_ref(buf)?;
            span.resource = buf_wrapper.create_no_alloc_string(value.as_bytes());
            *buf = next;
        }
        SpanKey::TraceId => span.trace_id = read_number(buf)?.try_into()?,
        SpanKey::SpanId => span.span_id = read_number(buf)?.try_into()?,
        SpanKey::ParentId => span.parent_id = read_number(buf)?.try_into()?,
        SpanKey::Start => span.start = read_number(buf)?.try_into()?,
        SpanKey::Duration => span.duration = read_number(buf)?.try_into()?,
        SpanKey::Error => span.error = read_number(buf)?.try_into()?,
        SpanKey::Type => {
            let (value, next) = read_string_ref(buf)?;
            span.r#type = buf_wrapper.create_no_alloc_string(value.as_bytes());
            *buf = next;
        }
        SpanKey::Meta => span.meta = read_str_map_to_no_alloc_strings(buf_wrapper, buf)?,
        SpanKey::Metrics => span.metrics = read_metrics(buf_wrapper, buf)?,
        SpanKey::MetaStruct => span.meta_struct = read_meta_struct(buf_wrapper, buf)?,
        SpanKey::SpanLinks => span.span_links = read_span_links(buf_wrapper, buf)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SpanKey;
    use std::str::FromStr;
    use crate::span_v04::SpanKeyParseError;

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

        assert!(matches!(
            SpanKey::from_str("invalid_key"),
            Err(SpanKeyParseError)
        ));
    }
}
