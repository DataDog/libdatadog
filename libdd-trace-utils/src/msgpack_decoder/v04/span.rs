// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::{Buffer, read_str_bytes_raw};
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_nullable_number;
use crate::msgpack_decoder::decode::span_event::read_span_events;
use crate::msgpack_decoder::decode::span_link::read_span_links;
use crate::msgpack_decoder::decode::string::{
    read_nullable_str_map_to_strings, read_nullable_string,
};
use crate::msgpack_decoder::decode::{meta_struct::read_meta_struct, metrics::read_metrics};
#[cfg(test)]
use crate::span::v04::SpanKey;
use crate::span::{DeserializableTraceData, v04::Span};

#[cfg(test)]
pub fn decode_span<T: DeserializableTraceData>(
    buffer: &mut Buffer<T>,
) -> Result<Span<T>, DecodeError> {
    let mut span = Span::<T>::default();
    decode_span_into(buffer, &mut span)?;
    Ok(span)
}

/// Decodes a msgpack span from `buffer` directly into `span`.
///
/// # Arguments
///
/// * `buffer` - A mutable reference to a buffer containing the encoded data.
/// * `span` - Destination `Span` (expected to be default-initialized).
///
/// # Errors
///
/// This function will return an error if:
/// - The map length cannot be read.
/// - Any key or value cannot be decoded.
#[inline]
pub(crate) fn decode_span_into<T: DeserializableTraceData>(
    buffer: &mut Buffer<T>,
    span: &mut Span<T>,
) -> Result<(), DecodeError> {
    let span_size = crate::msgpack_decoder::decode::map::read_map_len(buffer).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(span, buffer)?;
    }

    // Decoded from msgpack maps (unique keys) or empty defaults: no duplicates, so mark deduped to
    // skip the defensive dedup (and its warning) at encoding time. A later mutation re-dirties it.
    span.meta.mark_deduped();
    span.metrics.mark_deduped();
    span.meta_struct.mark_deduped();

    Ok(())
}

#[cold]
#[inline(never)]
fn invalid_span_key_error(key_bytes: &[u8]) -> DecodeError {
    match std::str::from_utf8(key_bytes) {
        Ok(s) => DecodeError::InvalidFormat(format!("Invalid span key: {s}")),
        Err(e) => DecodeError::Utf8Error(e.to_string()),
    }
}

#[inline(always)]
fn fill_span<T: DeserializableTraceData>(
    span: &mut Span<T>,
    buf: &mut Buffer<T>,
) -> Result<(), DecodeError> {
    let slice = buf.as_mut_slice();
    let (key_bytes, remaining) = read_str_bytes_raw(slice)?;
    *slice = remaining;

    match key_bytes {
        b"service" => span.service = read_nullable_string(buf)?,
        b"name" => span.name = read_nullable_string(buf)?,
        b"resource" => span.resource = read_nullable_string(buf)?,
        b"trace_id" => span.trace_id = read_nullable_number::<_, u64>(buf)? as u128,
        b"span_id" => span.span_id = read_nullable_number(buf)?,
        b"parent_id" => span.parent_id = read_nullable_number(buf)?,
        b"start" => span.start = read_nullable_number(buf)?,
        b"duration" => span.duration = read_nullable_number(buf)?,
        b"error" => span.error = read_nullable_number(buf)?,
        b"type" => span.r#type = read_nullable_string(buf)?,
        b"meta" => span.meta = read_nullable_str_map_to_strings(buf)?,
        b"metrics" => span.metrics = read_metrics(buf)?,
        b"meta_struct" => span.meta_struct = read_meta_struct(buf)?,
        b"span_links" => span.span_links = read_span_links(buf)?,
        b"span_events" => span.span_events = read_span_events(buf)?,
        _ => return Err(invalid_span_key_error(key_bytes)),
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
