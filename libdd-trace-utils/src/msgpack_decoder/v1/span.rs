// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::buffer::Buffer;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::number::read_nullable_number;
use crate::msgpack_decoder::decode::span_event::read_span_events;
use crate::msgpack_decoder::decode::span_link::read_span_links;
use crate::msgpack_decoder::decode::string::{
    read_nullable_str_map_to_strings, read_nullable_string,
};
use crate::msgpack_decoder::decode::{meta_struct::read_meta_struct, metrics::read_metrics};
use crate::span::{v04::Span, v04::SpanKey, DeserializableTraceData};
use std::borrow::Borrow;
use rmp::Marker;
use rmp::decode::{read_marker, RmpRead};

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
pub fn decode_span<T: DeserializableTraceData>(buffer: &mut Buffer<T>) -> Result<Span<T>, DecodeError> {
    let mut span = Span::<T>::default();

    let span_size = rmp::decode::read_map_len(buffer.as_mut_slice()).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(&mut span, buffer)?;
    }

    Ok(span)
}

fn read_trace_id(buf: &mut &'static [u8]) -> Result<u128, DecodeError> {
    let byte_array_len = match read_marker(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read marker for trace_id".to_owned())
    })? {
        Marker::Bin8 => Ok(u32::from(buf.read_data_u8().map_err(|_| {
            DecodeError::InvalidFormat("Unable to byte array size for trace_id".to_owned())
        })?)),
        Marker::Null => return Ok(0),
        _ => Err(DecodeError::InvalidFormat("trace_id is not Bin8 or Null".to_owned()))
    }?;

    if byte_array_len != 16 {
        return Err(DecodeError::InvalidFormat("trace_id must be exactly 16 bytes.".to_owned()))
    }

    if buf.len() < 16 {
        Err(DecodeError::InvalidFormat(
            "Invalid data length".to_string(),
        ))
    } else {
        let trace_id_buf;
        (trace_id_buf, *buf) = buf.split_at(16);
        Ok(u128::from_be_bytes(trace_id_buf.try_into().unwrap()))
    }
}

// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings
fn fill_span<T: DeserializableTraceData>(span: &mut Span<T>, buf: &mut Buffer<T>) -> Result<(), DecodeError> {
    let key = buf
        .read_string()?
        .borrow()
        .parse::<SpanKey>()
        .map_err(|e| DecodeError::InvalidFormat(e.message))?;

    match key {
        SpanKey::Service => span.service = read_nullable_string(buf)?,
        SpanKey::Name => span.name = read_nullable_string(buf)?,
        SpanKey::Resource => span.resource = read_nullable_string(buf)?,
        SpanKey::TraceId => span.trace_id = read_trace_id(buf.as_mut_slice())?,
        SpanKey::SpanId => span.span_id = read_nullable_number(buf)?,
        SpanKey::ParentId => span.parent_id = read_nullable_number(buf)?,
        SpanKey::Start => span.start = read_nullable_number(buf)?,
        SpanKey::Duration => span.duration = read_nullable_number(buf)?,
        SpanKey::Error => span.error = read_nullable_number(buf)?,
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
