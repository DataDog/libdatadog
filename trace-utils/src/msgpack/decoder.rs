// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack::error::DecodeError;
use crate::msgpack::number::read_number;
use crate::tracer_payload::TracerPayloadV04;
use datadog_trace_protobuf::pb::{Span, SpanLink};
use rmp::{
    decode::{read_array_len, RmpRead},
    Marker,
};
use std::{collections::HashMap, f64, str::FromStr};

#[inline]
pub fn read_string_ref(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
    rmp::decode::read_str_from_slice(buf).map_err(|_| DecodeError::WrongFormat)
}

#[inline]
pub fn read_string(buf: &mut &[u8]) -> Result<String, DecodeError> {
    let value_len: usize = rmp::decode::read_str_len(buf)
        .map_err(|_| DecodeError::WrongFormat)?
        .try_into()
        .map_err(|_| DecodeError::WrongConversion)?;

    let mut vec = vec![0; value_len];
    buf.read_exact_buf(vec.as_mut_slice())
        .map_err(|_| DecodeError::IOError)?;

    let str = String::from_utf8(vec).map_err(|_| DecodeError::Utf8Error)?;
    Ok(str)
}

#[inline]
fn read_str_pair(buf: &mut &[u8]) -> Result<(String, String), DecodeError> {
    let k = read_string(buf)?;
    let v = read_string(buf)?;

    Ok((k, v))
}

#[inline]
fn read_metric_pair(buf: &mut &[u8]) -> Result<(String, f64), DecodeError> {
    let k = read_string(buf)?;
    let v = read_number(buf)?.try_into()?;

    Ok((k, v))
}

fn read_map_strs(buf: &mut &[u8]) -> Result<HashMap<String, String>, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| DecodeError::WrongFormat)? {
        Marker::FixMap(len) => {
            let mut map = HashMap::new();
            for _ in 0..len {
                let (k, v) = read_str_pair(buf)?;
                map.insert(k, v);
            }
            Ok(map)
        }
        _ => Err(DecodeError::WrongType),
    }
}

fn read_metrics(buf: &mut &[u8]) -> Result<HashMap<String, f64>, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| DecodeError::WrongFormat)? {
        Marker::FixMap(len) => {
            let mut metrics = HashMap::new();
            for _ in 0..len {
                let (k, v) = read_metric_pair(buf)?;
                metrics.insert(k, v);
            }
            Ok(metrics)
        }
        _ => Err(DecodeError::WrongType),
    }
}

fn read_meta_struct(buf: &mut &[u8]) -> Result<HashMap<String, Vec<u8>>, DecodeError> {
    match rmp::decode::read_marker(buf).map_err(|_| DecodeError::WrongFormat)? {
        Marker::FixMap(len) => {
            let mut meta_struct = HashMap::new();
            for _ in 0..len {
                let k = read_string(buf)?;
                let mut v = vec![];
                let array_len =
                    rmp::decode::read_array_len(buf).map_err(|_| DecodeError::WrongFormat)?;
                for _ in 0..array_len {
                    let value = read_number(buf)?.try_into()?;
                    v.push(value);
                }
                meta_struct.insert(k, v);
            }
            Ok(meta_struct)
        }
        _ => Err(DecodeError::WrongType),
    }
}

#[allow(clippy::explicit_auto_deref)]
fn decode_span_link(buf: &mut &[u8]) -> Result<SpanLink, DecodeError> {
    let mut span = SpanLink::default();
    let span_size = rmp::decode::read_map_len(buf).map_err(|_| DecodeError::WrongType)?;

    for _ in 0..span_size {
        let (key, value) = read_string_ref(*buf)?;
        *buf = value;
        if key == "trace_id" {
            span.trace_id = read_number(buf)?.try_into()?;
        } else if key == "trace_id_high" {
            span.trace_id_high = read_number(buf)?.try_into()?;
        } else if key == "span_id" {
            span.span_id = read_number(buf)?.try_into()?;
        } else if key == "attributes" {
            span.attributes = read_map_strs(buf)?;
        } else if key == "tracestate" {
            let (value, next) = read_string_ref(*buf)?;
            span.tracestate = String::from_str(value).unwrap();
            *buf = next;
        } else if key == "flags" {
            span.flags = read_number(buf)?.try_into()?;
        } else {
            return Err(DecodeError::WrongFormat);
        }
    }

    Ok(span)
}

fn read_span_links(buf: &mut &[u8]) -> Result<Vec<SpanLink>, DecodeError> {
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

// Disabling explicit_auto_deref warning because passing buf instead of *buf to read_string_ref
// leads to borrow checker errors.
#[allow(clippy::explicit_auto_deref)]
fn fill_span(span: &mut Span, buf: &mut &[u8]) -> Result<(), DecodeError> {
    // field's key won't be held so no need to copy it in a buffer.
    let (key, value) = read_string_ref(*buf)?;

    // Go to the value
    *buf = value;
    if key == "service" {
        let (value, next) = read_string_ref(*buf)?;
        span.service = String::from_str(value).unwrap();
        *buf = next;
    } else if key == "name" {
        let (value, next) = read_string_ref(*buf)?;
        span.name = String::from_str(value).unwrap();
        *buf = next;
    } else if key == "resource" {
        let (value, next) = read_string_ref(*buf)?;
        span.resource = String::from_str(value).unwrap();
        *buf = next;
    } else if key == "trace_id" {
        span.trace_id = read_number(buf)?.try_into()?;
    } else if key == "span_id" {
        span.span_id = read_number(buf)?.try_into()?;
    } else if key == "parent_id" {
        span.parent_id = read_number(buf)?.try_into()?;
    } else if key == "start" {
        span.start = read_number(buf)?.try_into()?;
    } else if key == "duration" {
        span.duration = read_number(buf)?.try_into()?;
    } else if key == "error" {
        span.error = read_number(buf)?.try_into()?;
    } else if key == "meta" {
        span.meta = read_map_strs(buf)?;
    } else if key == "metrics" {
        span.metrics = read_metrics(buf)?;
    } else if key == "type" {
        let (value, next) = read_string_ref(*buf)?;
        span.r#type = String::from_str(value).unwrap();
        *buf = next;
    } else if key == "meta_struct" {
        span.meta_struct = read_meta_struct(buf)?;
    } else if key == "span_links" {
        span.span_links = read_span_links(buf)?;
    } else {
        return Err(DecodeError::WrongFormat);
    }
    Ok(())
}

#[inline]
fn decode_span_v04(buf: &mut &[u8]) -> Result<Span, DecodeError> {
    let mut span = Span::default();

    let span_size = rmp::decode::read_map_len(buf).unwrap();

    for _ in 0..span_size {
        fill_span(&mut span, buf)?;
    }
    Ok(span)
}

pub fn from_slice(data: &mut &[u8]) -> Result<Vec<TracerPayloadV04>, DecodeError> {
    let trace_count = read_array_len(data).map_err(|_| DecodeError::WrongFormat)?;

    let mut traces: Vec<TracerPayloadV04> = Default::default();

    for _ in 0..trace_count {
        let span_count = read_array_len(data).unwrap();
        let mut trace: Vec<Span> = Default::default();

        for _ in 0..span_count {
            let span = decode_span_v04(data)?;
            trace.push(span);
        }
        traces.push(trace);
    }

    Ok(traces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_read_string_succes() {
        let expected = "foobar".to_string();
        let payload = rmp_serde::to_vec(&expected).unwrap();

        assert_eq!(expected, read_string(&mut payload.as_ref()).unwrap());
    }

    #[test]
    fn decoder_read_string_wrong_format() {
        let input: [u8; 2] = [255; 2];

        assert_eq!(
            Err(DecodeError::WrongFormat),
            read_string(&mut input.as_ref())
        );
    }

    #[test]
    fn decoder_read_string_utf8_error() {
        let invalid_seq = vec![0, 159, 146, 150];
        let str = unsafe { String::from_utf8_unchecked(invalid_seq) };
        let payload = rmp_serde::to_vec(&str).unwrap();
        assert_eq!(
            Err(DecodeError::Utf8Error),
            read_string(&mut payload.as_ref())
        );
    }

    #[test]
    fn decoder_span_link_success() {
        let span_links = vec![SpanLink {
            trace_id: 1,
            trace_id_high: 0,
            span_id: 1,
            attributes: HashMap::from([
                ("attr1".to_string(), "test_value".to_string()),
                ("attr2".to_string(), "test_value".to_string()),
            ]),
            tracestate: "state_test".to_string(),
            flags: 0b101,
        }];

        let payload = rmp_serde::to_vec_named(&span_links).unwrap();

        assert_eq!(span_links, read_span_links(&mut payload.as_ref()).unwrap())
    }

    #[test]
    fn decoder_meta_struct_success() {
        let meta_struct = HashMap::from([
            ("key".to_string(), vec![1, 2, 3]),
            ("key2".to_string(), vec![4, 5, 6]),
        ]);

        let payload = rmp_serde::to_vec_named(&meta_struct).unwrap();

        assert_eq!(
            meta_struct,
            read_meta_struct(&mut payload.as_ref()).unwrap()
        )
    }

    #[test]
    fn decoder_meta_success() {
        let meta = HashMap::from([
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]);

        let payload = rmp_serde::to_vec_named(&meta).unwrap();

        assert_eq!(meta, read_map_strs(&mut payload.as_ref()).unwrap())
    }
}
