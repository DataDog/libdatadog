// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;
mod span_link;

use self::span::decode_span;
use super::error::DecodeError;
use super::number::read_number;
use crate::tracer_payload::TracerPayloadV04;
use datadog_trace_protobuf::pb::Span;
use rmp::{
    decode::{read_array_len, RmpRead},
    Marker,
};
use std::{collections::HashMap, f64};

pub fn from_slice(data: &mut &[u8]) -> Result<Vec<TracerPayloadV04>, DecodeError> {
    let trace_count = read_array_len(data).map_err(|_| DecodeError::WrongFormat)?;

    let mut traces: Vec<TracerPayloadV04> = Default::default();

    for _ in 0..trace_count {
        let span_count = read_array_len(data).unwrap();
        let mut trace: Vec<Span> = Default::default();

        for _ in 0..span_count {
            let span = decode_span(data)?;
            trace.push(span);
        }
        traces.push(trace);
    }

    Ok(traces)
}

#[inline]
fn read_string_ref(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
    rmp::decode::read_str_from_slice(buf).map_err(|_| DecodeError::WrongFormat)
}

#[inline]
fn read_string(buf: &mut &[u8]) -> Result<String, DecodeError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_decoder::v04::decoder::span_link::read_span_links;
    use datadog_trace_protobuf::pb::SpanLink;

    #[test]
    fn decoder_read_string_success() {
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
