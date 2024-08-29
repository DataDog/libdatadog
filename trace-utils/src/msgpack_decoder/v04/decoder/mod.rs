// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;
mod span_link;

use self::span::decode_span;
use super::error::DecodeError;
use super::number::read_number;
use crate::no_alloc_string::{BufferWrapper, NoAllocString};
use crate::span_v04::Span;
use rmp::decode::DecodeStringError;
use rmp::{decode, decode::RmpRead, Marker};
use std::{collections::HashMap, f64};

/// Decodes a slice of bytes into a vector of `TracerPayloadV04` objects.
///
///
///
/// # Arguments
///
/// * `data` - A tinybytes Bytes buffer containing the encoded data. Bytes are expected to be
///   encoded msgpack data containing a list of a list of v04 spans.
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
/// use tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_tinybytes = tinybytes::Bytes::from(encoded_data);
/// let decoded_traces = from_slice(encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name.as_str());
/// ```
pub fn from_slice(data: tinybytes::Bytes) -> Result<Vec<Vec<Span>>, DecodeError> {
    let mut local_buf = data.as_ref();
    let trace_count = rmp::decode::read_array_len(&mut local_buf).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read array len for trace count".to_owned())
    })?;

    (0..trace_count).try_fold(
        Vec::with_capacity(
            trace_count
                .try_into()
                .expect("Unable to cast trace_count to usize"),
        ),
        |mut traces, _| {
            let span_count = rmp::decode::read_array_len(&mut local_buf).map_err(|_| {
                DecodeError::InvalidFormat("Unable to read array len for span count".to_owned())
            })?;

            let trace = (0..span_count).try_fold(
                Vec::with_capacity(
                    span_count
                        .try_into()
                        .expect("Unable to cast span_count to usize"),
                ),
                |mut trace, _| {
                    let span = decode_span(&data, &mut local_buf)?;
                    trace.push(span);
                    Ok(trace)
                },
            )?;

            traces.push(trace);

            Ok(traces)
        },
    )
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
fn read_str_map_to_no_alloc_strings(
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<HashMap<NoAllocString, NoAllocString>, DecodeError> {
    let len = decode::read_map_len(buf)
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("TODO: EK"));
    for _ in 0..len {
        let (key, next) = read_string_ref(buf)?;
        *buf = next;

        let (val, next) = read_string_ref(buf)?;
        *buf = next;

        map.insert(
            buf_wrapper.create_no_alloc_string(key.as_bytes()),
            buf_wrapper.create_no_alloc_string(val.as_bytes()),
        );
    }
    Ok(map)
}

#[inline]
fn read_metric_pair(
    buffer_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<(NoAllocString, f64), DecodeError> {
    let (key, next) = read_string_ref(buf)?;
    *buf = next;

    let v = read_number(buf)?.try_into()?;

    Ok((buffer_wrapper.create_no_alloc_string(key.as_bytes()), v))
}
fn read_metrics(
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<HashMap<NoAllocString, f64>, DecodeError> {
    let len = read_map_len(buf)?;
    read_map(len, buf_wrapper, buf, read_metric_pair)
}

fn read_meta_struct(
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
) -> Result<HashMap<NoAllocString, Vec<u8>>, DecodeError> {
    fn read_meta_struct_pair(
        buf_wrapper: &BufferWrapper,
        buf: &mut &[u8],
    ) -> Result<(NoAllocString, Vec<u8>), DecodeError> {
        let (key, next) = read_string_ref(buf)?;
        *buf = next;
        let array_len = decode::read_array_len(buf).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;

        let mut v = Vec::with_capacity(array_len as usize);

        for _ in 0..array_len {
            let value = read_number(buf)?.try_into()?;
            v.push(value);
        }
        Ok((buf_wrapper.create_no_alloc_string(key.as_bytes()), v))
    }

    let len = read_map_len(buf)?;
    read_map(len, buf_wrapper, buf, read_meta_struct_pair)
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
// TODO: EK - Fix the documentation for this function
fn read_map<K, V, F>(
    len: usize,
    buf_wrapper: &BufferWrapper,
    buf: &mut &[u8],
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&BufferWrapper, &mut &[u8]) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf_wrapper, buf)?;
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
    use crate::no_alloc_string::NoAllocString;
    use crate::span_v04::SpanLink;
    use bolero::check;
    use rmp_serde;
    use rmp_serde::to_vec_named;

    #[test]
    fn decoder_read_string_success() {
        let expected_string = "test-service-name";
        let span = Span {
            name: NoAllocString::from_slice(expected_string.as_ref()),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_string, decoded_span.name.as_str());
    }

    #[test]
    fn test_decoder_meta_struct_fixed_map_success() {
        let expected_meta_struct = HashMap::from([
            (NoAllocString::from_slice("key1".as_ref()), vec![1, 2, 3]),
            (NoAllocString::from_slice("key2".as_ref()), vec![4, 5, 6]),
        ]);

        let span = Span {
            meta_struct: expected_meta_struct.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta_struct, decoded_span.meta_struct);
    }

    #[test]
    fn test_decoder_meta_struct_map_16_success() {
        let expected_meta_struct: HashMap<NoAllocString, Vec<u8>> = (0..20)
            .map(|i| {
                (
                    NoAllocString::from_slice(format!("key {}", i).as_ref()),
                    vec![1 + i, 2 + i, 3 + i],
                )
            })
            .collect();

        let span = Span {
            meta_struct: expected_meta_struct.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert_eq!(expected_meta_struct, decoded_span.meta_struct);
    }

    #[test]
    fn test_decoder_meta_fixed_map_success() {
        let expected_meta = HashMap::from([
            (
                NoAllocString::from_slice("key1".as_ref()),
                NoAllocString::from_slice("value1".as_ref()),
            ),
            (
                NoAllocString::from_slice("key2".as_ref()),
                NoAllocString::from_slice("value2".as_ref()),
            ),
        ]);
        let span = Span {
            meta: expected_meta.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_meta, decoded_span.meta);
    }

    #[test]
    fn test_decoder_meta_map_16_success() {
        let expected_meta: HashMap<NoAllocString, NoAllocString> = (0..20)
            .map(|i| {
                (
                    NoAllocString::from_slice(format!("key {}", i).as_ref()),
                    NoAllocString::from_slice(format!("value {}", i).as_ref()),
                )
            })
            .collect();

        let span = Span {
            meta: expected_meta.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert_eq!(expected_meta, decoded_span.meta);
    }

    #[test]
    fn test_decoder_metrics_fixed_map_success() {
        let mut span = Span::default();
        let expected_metrics = HashMap::from([
            (NoAllocString::from_slice("metric1".as_ref()), 1.23),
            (NoAllocString::from_slice("metric2".as_ref()), 4.56),
        ]);
        span.metrics = expected_metrics.clone();
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_metrics, decoded_span.metrics);
    }

    #[test]
    fn test_decoder_metrics_map16_success() {
        let mut span = Span::default();
        let expected_metrics: HashMap<NoAllocString, f64> = (0..20)
            .map(|i| {
                (
                    NoAllocString::from_slice(format!("metric{}", i).as_ref()),
                    i as f64,
                )
            })
            .collect();

        span.metrics = expected_metrics.clone();
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
                (
                    NoAllocString::from_slice("attr1".as_ref()),
                    NoAllocString::from_slice("test_value".as_ref()),
                ),
                (
                    NoAllocString::from_slice("attr2".as_ref()),
                    NoAllocString::from_slice("test_value2".as_ref()),
                ),
            ]),
            tracestate: NoAllocString::from_slice("state_test".as_ref()),
            flags: 0b101,
        }];

        let span = Span {
            span_links: expected_span_links.clone(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_span_links, decoded_span.span_links);
    }

    #[test]
    fn test_decoder_read_string_wrong_format() {
        let span = Span {
            service: NoAllocString::from_slice("my_service".as_ref()),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the map size from 11 to 12 to trigger an InvalidMarkerRead error.
        encoded_data[125] = 0x8c;

        let result = from_slice(tinybytes::Bytes::from(encoded_data));
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
            name: NoAllocString::from_slice(invalid_str.as_ref()),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();

        let result = from_slice(tinybytes::Bytes::from(encoded_data));
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

        let result = from_slice(tinybytes::Bytes::from(encoded_data));
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

        let result = from_slice(tinybytes::Bytes::from(encoded_data));
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Unable to read array len for span count".to_owned()
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

        let result = from_slice(tinybytes::Bytes::from(encoded_data));
        assert_eq!(
            Err(DecodeError::InvalidType(
                "Type mismatch at marker FixPos(1)".to_owned()
            )),
            result
        );
    }

    #[test]
    fn fuzz_from_slice() {
        check!()
            .with_type::<(
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                String,
                u64,
                u64,
                u64,
                i64,
            )>()
            .cloned()
            .for_each(
                |(
                    name,
                    service,
                    resource,
                    span_type,
                    meta_key,
                    meta_value,
                    metric_key,
                    metric_value,
                    trace_id,
                    span_id,
                    parent_id,
                    start,
                )| {
                    let span = Span {
                        name: NoAllocString::from_slice(name.as_ref()),
                        service: NoAllocString::from_slice(service.as_ref()),
                        resource: NoAllocString::from_slice(resource.as_ref()),
                        r#type: NoAllocString::from_slice(span_type.as_ref()),
                        meta: HashMap::from([(
                            NoAllocString::from_slice(meta_key.as_ref()),
                            NoAllocString::from_slice(meta_value.as_ref()),
                        )]),
                        metrics: HashMap::from([(
                            NoAllocString::from_slice(metric_key.as_ref()),
                            metric_value.parse::<f64>().unwrap_or_default(),
                        )]),
                        trace_id,
                        span_id,
                        parent_id,
                        start,
                        ..Default::default()
                    };
                    let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
                    let result = from_slice(tinybytes::Bytes::from(encoded_data));

                    assert!(result.is_ok());
                },
            );
    }
}
