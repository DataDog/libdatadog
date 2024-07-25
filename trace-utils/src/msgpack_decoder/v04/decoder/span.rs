// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{
    read_map_strs, read_meta_struct, read_metrics, read_string_ref, span_link::read_span_links,
};
use crate::msgpack_decoder::v04::error::DecodeError;
use crate::msgpack_decoder::v04::number::read_number;
use datadog_trace_protobuf::pb::Span;
use std::str::FromStr;

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
#[inline]
pub(crate) fn decode_span(buf: &mut &[u8]) -> Result<Span, DecodeError> {
    let mut span = Span::default();
    let span_size = rmp::decode::read_map_len(buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to get map len for span size".to_owned())
    })?;

    for _ in 0..span_size {
        fill_span(&mut span, buf)?;
    }
    Ok(span)
}

#[derive(Debug, PartialEq)]
enum SpanKey {
    Service,
    Name,
    Resource,
    TraceId,
    SpanId,
    ParentId,
    Start,
    Duration,
    Error,
    Meta,
    Metrics,
    Type,
    MetaStruct,
    SpanLinks,
}

impl FromStr for SpanKey {
    type Err = DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "service" => Ok(SpanKey::Service),
            "name" => Ok(SpanKey::Name),
            "resource" => Ok(SpanKey::Resource),
            "trace_id" => Ok(SpanKey::TraceId),
            "span_id" => Ok(SpanKey::SpanId),
            "parent_id" => Ok(SpanKey::ParentId),
            "start" => Ok(SpanKey::Start),
            "duration" => Ok(SpanKey::Duration),
            "error" => Ok(SpanKey::Error),
            "meta" => Ok(SpanKey::Meta),
            "metrics" => Ok(SpanKey::Metrics),
            "type" => Ok(SpanKey::Type),
            "meta_struct" => Ok(SpanKey::MetaStruct),
            "span_links" => Ok(SpanKey::SpanLinks),
            _ => Err(DecodeError::InvalidFormat(
                format!("Invalid span key: {}", s).to_owned(),
            )),
        }
    }
}

fn fill_span(span: &mut Span, buf: &mut &[u8]) -> Result<(), DecodeError> {
    // field's key won't be held so no need to copy it in a buffer.
    let (key, value) = read_string_ref(buf)?;

    // Go to the value
    *buf = value;

    let key = key.parse::<SpanKey>()?;

    match key {
        SpanKey::Service => {
            let (value, next) = read_string_ref(buf)?;
            span.service = String::from_str(value).unwrap();
            *buf = next;
        }
        SpanKey::Name => {
            let (value, next) = read_string_ref(buf)?;
            span.name = String::from_str(value).unwrap();
            *buf = next;
        }
        SpanKey::Resource => {
            let (value, next) = read_string_ref(buf)?;
            span.resource = String::from_str(value).unwrap();
            *buf = next;
        }
        SpanKey::TraceId => span.trace_id = read_number(buf)?.try_into()?,
        SpanKey::SpanId => span.span_id = read_number(buf)?.try_into()?,
        SpanKey::ParentId => span.parent_id = read_number(buf)?.try_into()?,
        SpanKey::Start => span.start = read_number(buf)?.try_into()?,
        SpanKey::Duration => span.duration = read_number(buf)?.try_into()?,
        SpanKey::Error => span.error = read_number(buf)?.try_into()?,
        SpanKey::Meta => span.meta = read_map_strs(buf)?,
        SpanKey::Metrics => span.metrics = read_metrics(buf)?,
        SpanKey::Type => {
            let (value, next) = read_string_ref(buf)?;
            span.r#type = String::from_str(value).unwrap();
            *buf = next;
        }
        SpanKey::MetaStruct => span.meta_struct = read_meta_struct(buf)?,
        SpanKey::SpanLinks => span.span_links = read_span_links(buf)?,
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::SpanKey;
    use crate::msgpack_decoder::v04::error::DecodeError;
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

        assert!(matches!(
            SpanKey::from_str("invalid_key"),
            Err(DecodeError::InvalidFormat(_))
        ));
    }
}
