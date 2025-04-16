// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_nullable_number_slice;
use crate::msgpack_decoder::decode::span_event::read_span_events;
use crate::msgpack_decoder::decode::span_link::read_span_links;
use crate::msgpack_decoder::decode::string::{
    read_nullable_str_map_to_strings, read_nullable_string, read_string_ref,
};
use crate::msgpack_decoder::decode::{meta_struct::read_meta_struct, metrics::read_metrics};
use crate::span::{SpanKey, SpanSlice};

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

// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings
fn fill_span<'a>(span: &mut SpanSlice<'a>, buf: &mut &'a [u8]) -> Result<(), DecodeError> {
    let key = read_string_ref(buf)?
        .parse::<SpanKey>()
        .map_err(|e| DecodeError::InvalidFormat(e.message))?;

    match key {
        SpanKey::Service => span.service = read_nullable_string(buf)?,
        SpanKey::Name => span.name = read_nullable_string(buf)?,
        SpanKey::Resource => span.resource = read_nullable_string(buf)?,
        SpanKey::TraceId => span.trace_id = read_nullable_number_slice(buf)?,
        SpanKey::SpanId => span.span_id = read_nullable_number_slice(buf)?,
        SpanKey::ParentId => span.parent_id = read_nullable_number_slice(buf)?,
        SpanKey::Start => span.start = read_nullable_number_slice(buf)?,
        SpanKey::Duration => span.duration = read_nullable_number_slice(buf)?,
        SpanKey::Error => span.error = read_nullable_number_slice(buf)?,
        SpanKey::Type => span.r#type = read_nullable_string(buf)?,
        SpanKey::Meta => span.meta = read_nullable_str_map_to_strings(buf)?,
        SpanKey::Metrics => span.metrics = read_metrics(buf)?,
        SpanKey::MetaStruct => span.meta_struct = read_meta_struct(buf)?,
        SpanKey::SpanLinks => span.span_links = read_span_links(buf)?,
        SpanKey::SpanEvents => span.span_events = read_span_events(buf)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SpanKey;
    use crate::span::SpanKeyParseError;
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
        assert_eq!(
            SpanKey::from_str("span_events").unwrap(),
            SpanKey::SpanEvents
        );

        let invalid_result = SpanKey::from_str("invalid_key");
        let msg = format!("SpanKeyParseError: Invalid span key: {}", "invalid_key");
        assert!(matches!(invalid_result, Err(SpanKeyParseError { .. })));
        assert_eq!(invalid_result.unwrap_err().to_string(), msg);
    }
}
