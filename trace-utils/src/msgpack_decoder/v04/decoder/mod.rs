// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;
mod span_link;

use self::span::decode_span;
use super::error::DecodeError;
use super::number::read_number;
use crate::tracer_payload::TracerPayloadV04;
use datadog_trace_protobuf::pb::Span;
use rmp::decode::DecodeStringError;
use rmp::{
    decode,
    decode::{read_array_len, RmpRead},
    Marker,
};
use std::{collections::HashMap, f64};

/// Decodes a slice of bytes into a vector of `TracerPayloadV04` objects.
///
///
///
/// # Arguments
///
/// * `data` - A mutable reference to a slice of bytes containing the encoded data.Bytes are
///   expected to be encoded msgpack data containing a list of a list of v04 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV04>)` - A vector of decoded `TracerPayloadV04` objects if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The array length for trace count or span count cannot be read.
/// - Any span cannot be decoded.
///
/// # Examples
///
/// ```
/// use datadog_trace_protobuf::pb::Span;
/// use datadog_trace_utils::msgpack_decoder::v04::decoder::from_slice;
/// use rmp_serde::to_vec_named;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name);
/// ```
pub fn from_slice(data: &mut &[u8]) -> Result<Vec<TracerPayloadV04>, DecodeError> {
    let trace_count = read_array_len(data).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read array len for trace count".to_owned())
    })?;

    let mut traces: Vec<TracerPayloadV04> = Default::default();

    for _ in 0..trace_count {
        let span_count = match read_array_len(data) {
            Ok(count) => count,
            Err(_) => {
                return Err(DecodeError::InvalidFormat(
                    "Unable to read array len for span count".to_owned(),
                ))
            }
        };

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
    decode::read_str_from_slice(buf).map_err(|e| match e {
        DecodeStringError::InvalidMarkerRead(e) => DecodeError::InvalidFormat(e.to_string()),
        DecodeStringError::InvalidDataRead(e) => DecodeError::InvalidConversion(e.to_string()),
        DecodeStringError::TypeMismatch(marker) => {
            DecodeError::InvalidType(format!("Type mismatch at marker {:?}", marker))
        }
        DecodeStringError::InvalidUtf8(_, e) => DecodeError::Utf8Error(e.to_string()),
        _ => DecodeError::IOError,
    })
}

#[inline]
fn read_string(buf: &mut &[u8]) -> Result<String, DecodeError> {
    let value_len: usize = decode::read_str_len(buf)
        .map_err(|e| DecodeError::InvalidFormat(e.to_string()))?
        .try_into()
        .map_err(|_| {
            DecodeError::InvalidConversion("unable to get len of string buffer".to_owned())
        })?;

    let mut vec = vec![0; value_len];
    buf.read_exact_buf(vec.as_mut_slice())
        .map_err(|_| DecodeError::IOError)?;

    let str = String::from_utf8(vec).map_err(|e| DecodeError::Utf8Error(e.to_string()))?;
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
    let len = read_map_len(buf)?;
    read_map(len, buf, read_str_pair)
}

fn read_metrics(buf: &mut &[u8]) -> Result<HashMap<String, f64>, DecodeError> {
    let len = read_map_len(buf)?;
    read_map(len, buf, read_metric_pair)
}

fn read_meta_struct(buf: &mut &[u8]) -> Result<HashMap<String, Vec<u8>>, DecodeError> {
    fn read_meta_struct_pair(buf: &mut &[u8]) -> Result<(String, Vec<u8>), DecodeError> {
        let k = read_string(buf)?;
        let mut v = vec![];
        let array_len = decode::read_array_len(buf).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;
        for _ in 0..array_len {
            let value = read_number(buf)?.try_into()?;
            v.push(value);
        }
        Ok((k, v))
    }

    let len = read_map_len(buf)?;
    read_map(len, buf, read_meta_struct_pair)
}

/// Reads a map from the buffer and returns it as a `HashMap`.
///
/// This function is generic over the key and value types of the map, and it uses a provided
/// function to read key-value pairs from the buffer.
///
/// # Arguments
///
/// * `len` - The number of key-value pairs to read from the buffer.
/// * `buf` - A mutable reference to the buffer containing the encoded map data.
/// * `read_pair` - A function that reads a key-value pair from the buffer and returns it as a
///   `Result<(K, V), DecodeError>`.
///
/// # Returns
///
/// * `Ok(HashMap<K, V>)` - A `HashMap` containing the decoded key-value pairs if successful.
/// * `Err(DecodeError)` - An error if the decoding process fails.
///
/// # Errors
///
/// This function will return an error if:
/// - The `read_pair` function returns an error while reading a key-value pair.
///
/// # Type Parameters
///
/// * `K` - The type of the keys in the map. Must implement `std::hash::Hash` and `Eq`.
/// * `V` - The type of the values in the map.
/// * `F` - The type of the function used to read key-value pairs from the buffer.
fn read_map<K, V, F>(
    len: usize,
    buf: &mut &[u8],
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut &[u8]) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::new();
    for _ in 0..len {
        let (k, v) = read_pair(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

fn read_map_len(buf: &mut &[u8]) -> Result<usize, DecodeError> {
    match decode::read_marker(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to read marker for map".to_owned()))?
    {
        Marker::FixMap(len) => Ok(len as usize),
        Marker::Map16 => buf
            .read_data_u16()
            .map_err(|_| DecodeError::IOError)
            .map(|len| len as usize),
        Marker::Map32 => buf
            .read_data_u32()
            .map_err(|_| DecodeError::IOError)
            .map(|len| len as usize),
        _ => Err(DecodeError::InvalidType(
            "Unable to read map from buffer".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_protobuf::pb::SpanLink;

    #[test]
    fn decoder_read_string_success() {
        let expected_string = "test-service-name";
        let span = Span {
            name: expected_string.to_owned(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_string, decoded_span.name);
    }

    #[test]
    fn test_decoder_meta_struct_fixed_map_success() {
        let expected_meta_struct = HashMap::from([
            ("key1".to_string(), vec![1, 2, 3]),
            ("key2".to_string(), vec![4, 5, 6]),
        ]);
        let span = Span {
            meta_struct: expected_meta_struct.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta_struct, decoded_span.meta_struct);
    }

    #[test]
    fn test_decoder_meta_struct_map_16_success() {
        let expected_meta_struct: HashMap<String, Vec<u8>> = (0..20)
            .map(|i| (format!("key {}", i), vec![1 + i, 2 + i, 3 + i]))
            .collect();

        let span = Span {
            meta_struct: expected_meta_struct.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta_struct, decoded_span.meta_struct);
    }

    #[test]
    fn test_decoder_meta_fixed_map_success() {
        let expected_meta = HashMap::from([
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]);
        let span = Span {
            meta: expected_meta.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta, decoded_span.meta);
    }

    #[test]
    fn test_decoder_meta_map_16_success() {
        let expected_meta: HashMap<String, String> = (0..20)
            .map(|i| (format!("key {}", i), format!("value {}", i)))
            .collect();

        let span = Span {
            meta: expected_meta.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta, decoded_span.meta);
    }

    #[test]
    fn test_decoder_metrics_fixed_map_success() {
        let mut span = Span::default();
        let expected_metrics =
            HashMap::from([("metric1".to_string(), 1.23), ("metric2".to_string(), 4.56)]);
        span.metrics = expected_metrics.clone();
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_metrics, decoded_span.metrics);
    }

    #[test]
    fn test_decoder_metrics_map16_success() {
        let mut span = Span::default();
        let expected_metrics: HashMap<String, f64> = (0..20)
            .map(|i| (format!("metric{}", i), i as f64))
            .collect();

        span.metrics = expected_metrics.clone();
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces = from_slice(&mut encoded_data.as_slice()).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_metrics, decoded_span.metrics);
    }

    #[test]
    fn test_decoder_span_link_success() {
        let expected_span_links = vec![SpanLink {
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

        let span = Span {
            span_links: expected_span_links.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(&mut encoded_data.as_slice()).expect("unable to decode span");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_span_links, decoded_span.span_links);
    }

    #[test]
    fn test_decoder_read_string_wrong_format() {
        let span = Span {
            service: "my_service".to_owned(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the map size from 11 to 12 to trigger an InvalidMarkerRead error.
        encoded_data[2] = 0x8c;

        let result = from_slice(&mut encoded_data.as_slice());
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Expected at least bytes 1, but only got 0 (pos 0)".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_read_string_utf8_error() {
        let invalid_seq = vec![0, 159, 146, 150];
        let invalid_str = unsafe { String::from_utf8_unchecked(invalid_seq) };
        let span = Span {
            name: invalid_str.to_owned(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();

        let result = from_slice(&mut encoded_data.as_slice());
        assert_eq!(
            Err(DecodeError::Utf8Error(
                "invalid utf-8 sequence of 1 bytes from index 1".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_invalid_marker_for_trace_count_read() {
        let span = Span::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the entire payload to a map with 12 keys in order to trigger an error when
        // reading the array len of traces
        encoded_data[0] = 0x8c;

        let result = from_slice(&mut encoded_data.as_ref());
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Unable to read array len for trace count".to_string()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_invalid_marker_for_span_count_read() {
        let span = Span::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the entire payload to a map with 12 keys in order to trigger an error when
        // reading the array len of spans
        encoded_data[1] = 0x8c;

        let result = from_slice(&mut encoded_data.as_ref());
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Unable to read array len for span count".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_read_string_invalid_data_read() {
        let span = Span {
            name: "test-span".to_owned(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the marker for the empty metrics map to a str8 marker
        encoded_data[104] = 0xD9;

        let result = from_slice(&mut encoded_data.as_slice());
        assert_eq!(
            Err(DecodeError::InvalidConversion(
                "Expected at least bytes 1, but only got 0 (pos 1)".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_read_string_type_mismatch() {
        let span = Span::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // Modify the encoded data to cause a type mismatch by changing the marker for the `name`
        // field to an integer marker
        encoded_data[3] = 0x01;

        let result = from_slice(&mut encoded_data.as_slice());
        assert_eq!(
            Err(DecodeError::InvalidType(
                "Type mismatch at marker FixPos(1)".to_owned()
            )),
            result
        );
    }
}
