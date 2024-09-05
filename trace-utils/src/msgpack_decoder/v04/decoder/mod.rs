// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;
mod span_link;

use self::span::decode_span;
use super::error::DecodeError;
use super::number::read_number;
use crate::span_v04::Span;
use rmp::decode::DecodeStringError;
use rmp::{decode, decode::RmpRead, Marker};
use std::{collections::HashMap, f64};
use tinybytes::bytes_string::{BufferWrapper, BytesString};

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
/// assert_eq!("test-span", decoded_span.name.as_str().unwrap());
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
// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings.
fn read_str_map_to_bytes_strings(
    buf_wrapper: &mut BufferWrapper,
) -> Result<HashMap<BytesString, BytesString>, DecodeError> {
    let len = decode::read_map_len(buf_wrapper.underlying)
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let (key, next) = read_string_ref(buf_wrapper.underlying)?;
        *buf_wrapper.underlying = next;

        let (val, next) = read_string_ref(buf_wrapper.underlying)?;
        *buf_wrapper.underlying = next;

        let key = unsafe { buf_wrapper.create_bytes_string_unchecked(key.as_bytes()) };
        let value = unsafe { buf_wrapper.create_bytes_string_unchecked(val.as_bytes()) };
        map.insert(key, value);
    }
    Ok(map)
}

#[inline]
fn read_metric_pair(buf_wrapper: &mut BufferWrapper) -> Result<(BytesString, f64), DecodeError> {
    let (key, next) = read_string_ref(buf_wrapper.underlying)?;
    *buf_wrapper.underlying = next;

    let v = read_number(buf_wrapper.underlying)?.try_into()?;
    let key = unsafe { buf_wrapper.create_bytes_string_unchecked(key.as_bytes()) };

    Ok((key, v))
}
fn read_metrics(buf_wrapper: &mut BufferWrapper) -> Result<HashMap<BytesString, f64>, DecodeError> {
    let len = read_map_len(buf_wrapper.underlying)?;
    read_map(len, buf_wrapper, read_metric_pair)
}

fn read_meta_struct(
    buf_wrapper: &mut BufferWrapper,
) -> Result<HashMap<BytesString, Vec<u8>>, DecodeError> {
    fn read_meta_struct_pair(
        buf_wrapper: &mut BufferWrapper,
    ) -> Result<(BytesString, Vec<u8>), DecodeError> {
        let (key, next) = read_string_ref(buf_wrapper.underlying)?;
        *buf_wrapper.underlying = next;
        let array_len = decode::read_array_len(buf_wrapper.underlying).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;

        let mut v = Vec::with_capacity(array_len as usize);

        for _ in 0..array_len {
            let value = read_number(buf_wrapper.underlying)?.try_into()?;
            v.push(value);
        }
        let key = unsafe { buf_wrapper.create_bytes_string_unchecked(key.as_bytes()) };
        Ok((key, v))
    }

    let len = read_map_len(buf_wrapper.underlying)?;
    read_map(len, buf_wrapper, read_meta_struct_pair)
}

/// Reads a map from the buffer and returns it as a `HashMap`.
///
/// This function is generic over the key and value types of the map, and it uses a provided
/// function to read key-value pairs from the buffer.
///
/// # Arguments
///
/// * `len` - The number of key-value pairs to read from the buffer.
/// * `buf_wrapper` - A reference to the BufferWrapper containing the encoded map data.
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
    buf_wrapper: &mut BufferWrapper,
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut BufferWrapper) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf_wrapper)?;
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
    use crate::test_utils::create_test_json_span;
    use bolero::check;
    use rmp_serde;
    use rmp_serde::to_vec_named;
    use serde_json::json;
    use tinybytes::bytes_string::BytesString;

    fn generate_meta_struct_element(i: u8) -> (String, Vec<u8>) {
        let map = HashMap::from([
            (
                format!("meta_struct_map_key {}", i + 1),
                format!("meta_struct_map_val {}", i + 1),
            ),
            (
                format!("meta_struct_map_key {}", i + 2),
                format!("meta_struct_map_val {}", i + 2),
            ),
        ]);
        let key = format!("key {}", i).to_owned();

        (key, rmp_serde::to_vec_named(&map).unwrap())
    }

    #[test]
    fn decoder_read_string_success() {
        let expected_string = "test-service-name";
        let span = Span {
            name: BytesString::from_slice(expected_string.as_ref()).unwrap(),
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_string, decoded_span.name.as_str().unwrap());
    }

    #[test]
    fn test_decoder_meta_struct_fixed_map_success() {
        let expected_meta_struct = HashMap::from([
            generate_meta_struct_element(0),
            generate_meta_struct_element(1),
        ]);

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta_struct"] = json!(expected_meta_struct.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta_struct.iter() {
            assert_eq!(
                value,
                &decoded_span.meta_struct[&BytesString::from_slice(key.as_ref()).unwrap()]
            );
        }
    }

    #[test]
    fn test_decoder_meta_struct_map_16_success() {
        let expected_meta_struct: HashMap<String, Vec<u8>> =
            (0..20).map(generate_meta_struct_element).collect();

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta_struct"] = json!(expected_meta_struct.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta_struct.iter() {
            assert_eq!(
                value,
                &decoded_span.meta_struct[&BytesString::from_slice(key.as_ref()).unwrap()]
            );
        }
    }

    #[test]
    fn test_decoder_meta_fixed_map_success() {
        let expected_meta = HashMap::from([
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]);

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta"] = json!(expected_meta.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta.iter() {
            assert_eq!(
                value,
                &decoded_span.meta[&BytesString::from_slice(key.as_ref()).unwrap()]
                    .as_str()
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_decoder_meta_map_16_success() {
        let expected_meta: HashMap<String, String> = (0..20)
            .map(|i| {
                (
                    format!("key {}", i).to_owned(),
                    format!("value {}", i).to_owned(),
                )
            })
            .collect();

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta"] = json!(expected_meta.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta.iter() {
            assert_eq!(
                value,
                &decoded_span.meta[&BytesString::from_slice(key.as_ref()).unwrap()]
                    .as_str()
                    .unwrap()
            );
        }
    }

    #[test]
    fn test_decoder_metrics_fixed_map_success() {
        let expected_metrics = HashMap::from([("metric1", 1.23), ("metric2", 4.56)]);

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["metrics"] = json!(expected_metrics.clone());
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_metrics.iter() {
            assert_eq!(
                value,
                &decoded_span.metrics[&BytesString::from_slice(key.as_ref()).unwrap()]
            );
        }
    }

    #[test]
    fn test_decoder_metrics_map16_success() {
        let expected_metrics: HashMap<String, f64> = (0..20)
            .map(|i| (format!("metric{}", i), i as f64))
            .collect();

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["metrics"] = json!(expected_metrics.clone());
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_metrics.iter() {
            assert_eq!(
                value,
                &decoded_span.metrics[&BytesString::from_slice(key.as_ref()).unwrap()]
            );
        }
    }

    #[test]
    fn test_decoder_span_link_success() {
        let expected_span_link = json!({
            "trace_id": 1,
            "trace_id_high": 0,
            "span_id": 1,
            "attributes": {
                "attr1": "test_value",
                "attr2": "test_value2"
            },
            "tracestate": "state_test",
            "flags": 0b101
        });

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["span_links"] = json!([expected_span_link]);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let decoded_traces =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert_eq!(
            expected_span_link["trace_id"],
            decoded_span.span_links[0].trace_id
        );
        assert_eq!(
            expected_span_link["trace_id_high"],
            decoded_span.span_links[0].trace_id_high
        );
        assert_eq!(
            expected_span_link["span_id"],
            decoded_span.span_links[0].span_id
        );
        assert_eq!(
            expected_span_link["tracestate"],
            decoded_span.span_links[0].tracestate.as_str().unwrap()
        );
        assert_eq!(
            expected_span_link["flags"],
            decoded_span.span_links[0].flags
        );
        assert_eq!(
            expected_span_link["attributes"]["attr1"],
            decoded_span.span_links[0].attributes
                [&BytesString::from_slice("attr1".as_ref()).unwrap()]
                .as_str()
                .unwrap()
        );
        assert_eq!(
            expected_span_link["attributes"]["attr2"],
            decoded_span.span_links[0].attributes
                [&BytesString::from_slice("attr2".as_ref()).unwrap()]
                .as_str()
                .unwrap()
        );
    }

    #[test]
    fn test_decoder_read_string_wrong_format() {
        let span = Span {
            service: BytesString::from_slice("my_service".as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the map size from 11 to 12 to trigger an InvalidMarkerRead error.
        encoded_data[2] = 0x8c;

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
        let invalid_str_as_bytes = tinybytes::Bytes::from(invalid_str);
        let span = Span {
            name: BytesString::from_bytes_unchecked(invalid_str_as_bytes),
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
                        name: BytesString::from_slice(name.as_ref()).unwrap(),
                        service: BytesString::from_slice(service.as_ref()).unwrap(),
                        resource: BytesString::from_slice(resource.as_ref()).unwrap(),
                        r#type: BytesString::from_slice(span_type.as_ref()).unwrap(),
                        meta: HashMap::from([(
                            BytesString::from_slice(meta_key.as_ref()).unwrap(),
                            BytesString::from_slice(meta_value.as_ref()).unwrap(),
                        )]),
                        metrics: HashMap::from([(
                            BytesString::from_slice(metric_key.as_ref()).unwrap(),
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
