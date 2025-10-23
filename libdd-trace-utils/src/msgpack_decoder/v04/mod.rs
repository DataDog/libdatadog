// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod span;

use self::span::decode_span;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::span::{SpanBytes, SpanSlice};

/// Decodes a Bytes buffer into a `Vec<Vec<SpanBytes>>` object, also represented as a vector of
/// `TracerPayloadV04` objects.
///
/// # Arguments
///
/// * `data` - A tinybytes Bytes buffer containing the encoded data. Bytes are expected to be
///   encoded msgpack data containing a list of a list of v04 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV04>, usize)` - A vector of decoded `Vec<SpanSlice>` objects if
///   successful. and the number of bytes in the slice used by the decoder.
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
/// use datadog_trace_utils::msgpack_decoder::v04::from_bytes;
/// use rmp_serde::to_vec_named;
/// use tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_tinybytes = tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_bytes(encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name.as_str());
/// ```
pub fn from_bytes(data: tinybytes::Bytes) -> Result<(Vec<Vec<SpanBytes>>, usize), DecodeError> {
    let mut parsed_data = data.clone();
    let (traces_ref, size) = from_slice(unsafe { parsed_data.as_mut_slice() })?;

    #[allow(clippy::unwrap_used)]
    let traces_owned = traces_ref
        .iter()
        .map(|trace| {
            trace
                .iter()
                // Safe to unwrap since the spans use subslices of the `data` slice
                .map(|span| span.try_to_bytes(&data).unwrap())
                .collect()
        })
        .collect();
    Ok((traces_owned, size))
}

/// Decodes a slice of bytes into a `Vec<Vec<SpanSlice>>` object.
/// The resulting spans have the same lifetime as the initial buffer.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing the encoded data. Bytes are expected to be encoded
///   msgpack data containing a list of a list of v04 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV04>, usize)` - A vector of decoded `Vec<SpanSlice>` objects if
///   successful. and the number of bytes in the slice used by the decoder.
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
/// use datadog_trace_utils::msgpack_decoder::v04::from_slice;
/// use rmp_serde::to_vec_named;
/// use tinybytes;
///
/// let span = Span {
///     name: "test-span".to_owned(),
///     ..Default::default()
/// };
/// let encoded_data = to_vec_named(&vec![vec![span]]).unwrap();
/// let encoded_data_as_tinybytes = tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_slice(&encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name);
/// ```
pub fn from_slice(mut data: &[u8]) -> Result<(Vec<Vec<SpanSlice<'_>>>, usize), DecodeError> {
    let trace_count = rmp::decode::read_array_len(&mut data).map_err(|_| {
        DecodeError::InvalidFormat("Unable to read array len for trace count".to_owned())
    })?;

    let start_len = data.len();

    #[allow(clippy::expect_used)]
    Ok((
        (0..trace_count).try_fold(
            Vec::with_capacity(
                trace_count
                    .try_into()
                    .expect("Unable to cast trace_count to usize"),
            ),
            |mut traces, _| {
                let span_count = rmp::decode::read_array_len(&mut data).map_err(|_| {
                    DecodeError::InvalidFormat("Unable to read array len for span count".to_owned())
                })?;

                let trace = (0..span_count).try_fold(
                    Vec::with_capacity(
                        span_count
                            .try_into()
                            .expect("Unable to cast span_count to usize"),
                    ),
                    |mut trace, _| {
                        let span = decode_span(&mut data)?;
                        trace.push(span);
                        Ok(trace)
                    },
                )?;

                traces.push(trace);

                Ok(traces)
            },
        )?,
        start_len - data.len(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{create_test_json_span, create_test_no_alloc_span};
    use bolero::check;
    use rmp_serde;
    use rmp_serde::to_vec_named;
    use serde_json::json;
    use std::collections::HashMap;
    use tinybytes::{Bytes, BytesString};

    #[test]
    fn test_empty_array() {
        let encoded_data = vec![0x90];
        let slice = encoded_data.as_ref();
        let (_decoded_traces, decoded_size) = from_slice(slice).expect("Decoding failed");

        assert_eq!(0, decoded_size);
    }

    #[test]
    fn test_decoder_size() {
        let span = SpanBytes {
            name: BytesString::from_slice("span_name".as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let expected_size = encoded_data.len() - 1; // rmp_serde adds additional 0 byte
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (_decoded_traces, decoded_size) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(expected_size, decoded_size);
    }

    #[test]
    fn test_decoder_read_string_success() {
        let expected_string = "test-service-name";
        let span = SpanBytes {
            name: BytesString::from_slice(expected_string.as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_string, decoded_span.name.as_str());
    }

    #[test]
    fn test_decoder_read_null_string_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["name"] = json!(null);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!("", decoded_span.name.as_str());
    }

    #[test]
    fn test_decoder_read_number_success() {
        let span = create_test_json_span(1, 2, 0, 0, false);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(1, decoded_span.trace_id);
    }

    #[test]
    fn test_decoder_read_null_number_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["trace_id"] = json!(null);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(0, decoded_span.trace_id);
    }

    #[test]
    fn test_decoder_meta_struct_null_map_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["meta_struct"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert!(decoded_span.meta_struct.is_empty());
    }

    #[test]
    fn test_decoder_meta_struct_success() {
        let data = vec![1, 2, 3, 4];
        let mut span = create_test_no_alloc_span(1, 2, 0, 0, false);
        span.meta_struct =
            HashMap::from([(BytesString::from("meta_key"), Bytes::from(data.clone()))]);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert_eq!(
            decoded_span.meta_struct.get("meta_key").unwrap().to_vec(),
            data
        );
    }

    #[test]
    fn test_decoder_meta_fixed_map_success() {
        let expected_meta = HashMap::from([
            ("key1".to_string(), "value1".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]);

        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["meta"] = json!(expected_meta.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta.iter() {
            assert_eq!(
                value,
                &decoded_span.meta[&BytesString::from_slice(key.as_ref()).unwrap()].as_str()
            );
        }
    }

    #[test]
    fn test_decoder_meta_null_map_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["meta"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert!(decoded_span.meta.is_empty());
    }

    #[test]
    fn test_decoder_meta_map_16_success() {
        let expected_meta: HashMap<String, String> = (0..20)
            .map(|i| {
                (
                    format!("key {i}").to_owned(),
                    format!("value {i}").to_owned(),
                )
            })
            .collect();

        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["meta"] = json!(expected_meta.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        for (key, value) in expected_meta.iter() {
            assert_eq!(
                value,
                &decoded_span.meta[&BytesString::from_slice(key.as_ref()).unwrap()].as_str()
            );
        }
    }

    #[test]
    fn test_decoder_metrics_fixed_map_success() {
        let expected_metrics = HashMap::from([("metric1", 1.23), ("metric2", 4.56)]);

        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["metrics"] = json!(expected_metrics.clone());
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
        let expected_metrics: HashMap<String, f64> =
            (0..20).map(|i| (format!("metric{i}"), i as f64)).collect();

        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["metrics"] = json!(expected_metrics.clone());
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
    fn test_decoder_metrics_null_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["metrics"] = json!(null);
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert!(decoded_span.metrics.is_empty());
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

        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["span_links"] = json!([expected_span_link]);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
            decoded_span.span_links[0].tracestate.as_str()
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
        );
        assert_eq!(
            expected_span_link["attributes"]["attr2"],
            decoded_span.span_links[0].attributes
                [&BytesString::from_slice("attr2".as_ref()).unwrap()]
                .as_str()
        );
    }

    #[test]
    fn test_decoder_null_span_link_success() {
        let mut span = create_test_json_span(1, 2, 0, 0, false);
        span["span_links"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_bytes(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert!(decoded_span.span_links.is_empty());
    }

    #[test]
    fn test_decoder_read_string_wrong_format() {
        let span = SpanBytes {
            service: BytesString::from_slice("my_service".as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the map size from 11 to 12 to trigger an InvalidMarkerRead error.
        encoded_data[2] = 0x8c;
        let slice = encoded_data.as_ref();

        let result = from_slice(slice);
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
        let span = SpanBytes {
            name: unsafe { BytesString::from_bytes_unchecked(invalid_str_as_bytes) },
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let slice = encoded_data.as_ref();

        let result = from_slice(slice);
        assert_eq!(
            Err(DecodeError::Utf8Error(
                "invalid utf-8 sequence of 1 bytes from index 1".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_invalid_marker_for_trace_count_read() {
        let span = SpanBytes::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the entire payload to a map with 12 keys in order to trigger an error when
        // reading the array len of traces
        encoded_data[0] = 0x8c;
        let slice = encoded_data.as_ref();

        let result = from_slice(slice);
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Unable to read array len for trace count".to_string()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_invalid_marker_for_span_count_read() {
        let span = SpanBytes::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // This changes the entire payload to a map with 12 keys in order to trigger an error when
        // reading the array len of spans
        encoded_data[1] = 0x8c;
        let slice = encoded_data.as_ref();

        let result = from_slice(slice);
        assert_eq!(
            Err(DecodeError::InvalidFormat(
                "Unable to read array len for span count".to_owned()
            )),
            result
        );
    }

    #[test]
    fn test_decoder_read_string_type_mismatch() {
        let span = SpanBytes::default();
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        // Modify the encoded data to cause a type mismatch by changing the marker for the `name`
        // field to an integer marker
        encoded_data[3] = 0x01;
        let slice = encoded_data.as_ref();

        let result = from_slice(slice);
        assert_eq!(
            Err(DecodeError::InvalidType(
                "Type mismatch at marker FixPos(1)".to_owned()
            )),
            result
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn fuzz_from_bytes() {
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
                    let span = SpanBytes {
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
                    let result = from_bytes(tinybytes::Bytes::from(encoded_data));

                    assert!(result.is_ok());
                },
            );
    }
}
