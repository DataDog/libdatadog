// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod span;
mod span_link;

use self::span::decode_span;
use super::error::DecodeError;
use super::number::read_number_bytes;
use crate::span_v04::Span;
use rmp::decode::DecodeStringError;
use rmp::{decode, decode::RmpRead, Marker};
use std::{collections::HashMap, f64};
use tinybytes::{Bytes, BytesString};

// https://docs.rs/rmp/latest/rmp/enum.Marker.html#variant.Null (0xc0 == 192)
const NULL_MARKER: &u8 = &0xc0;

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
/// let (decoded_traces, _payload_size) =
///     from_slice(encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("test-span", decoded_span.name.as_str());
/// ```
pub fn from_slice(mut data: tinybytes::Bytes) -> Result<(Vec<Vec<Span>>, usize), DecodeError> {
    let trace_count =
        rmp::decode::read_array_len(unsafe { data.as_mut_slice() }).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for trace count".to_owned())
        })?;

    let start_len = data.len();

    Ok((
        (0..trace_count).try_fold(
            Vec::with_capacity(
                trace_count
                    .try_into()
                    .expect("Unable to cast trace_count to usize"),
            ),
            |mut traces, _| {
                let span_count = rmp::decode::read_array_len(unsafe { data.as_mut_slice() })
                    .map_err(|_| {
                        DecodeError::InvalidFormat(
                            "Unable to read array len for span count".to_owned(),
                        )
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

#[inline]
fn read_string_ref_nomut(buf: &[u8]) -> Result<(&str, &[u8]), DecodeError> {
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
fn read_string_ref<'a>(buf: &mut &'a [u8]) -> Result<&'a str, DecodeError> {
    read_string_ref_nomut(buf).map(|(str, newbuf)| {
        *buf = newbuf;
        str
    })
}

#[inline]
fn read_string_bytes(buf: &mut Bytes) -> Result<BytesString, DecodeError> {
    // Note: we need to pass a &'static lifetime here, otherwise it'll complain
    read_string_ref_nomut(unsafe { buf.as_mut_slice() }).map(|(str, newbuf)| {
        let string = BytesString::from_bytes_slice(buf, str);
        *unsafe { buf.as_mut_slice() } = newbuf;
        string
    })
}

#[inline]
fn read_nullable_string_bytes(buf: &mut Bytes) -> Result<BytesString, DecodeError> {
    if let Some(empty_string) = handle_null_marker(buf, BytesString::default) {
        Ok(empty_string)
    } else {
        read_string_bytes(buf)
    }
}

#[inline]
// Safety: read_string_ref checks utf8 validity, so we don't do it again when creating the
// BytesStrings.
fn read_str_map_to_bytes_strings(
    buf: &mut Bytes,
) -> Result<HashMap<BytesString, BytesString>, DecodeError> {
    let len = decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = read_string_bytes(buf)?;
        let value = read_string_bytes(buf)?;
        map.insert(key, value);
    }
    Ok(map)
}

#[inline]
fn read_nullable_str_map_to_bytes_strings(
    buf: &mut Bytes,
) -> Result<HashMap<BytesString, BytesString>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    read_str_map_to_bytes_strings(buf)
}

#[inline]
fn read_metric_pair(buf: &mut Bytes) -> Result<(BytesString, f64), DecodeError> {
    let key = read_string_bytes(buf)?;
    let v = read_number_bytes(buf)?;

    Ok((key, v))
}
#[inline]
fn read_metrics(buf: &mut Bytes) -> Result<HashMap<BytesString, f64>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;

    read_map(len, buf, read_metric_pair)
}

#[inline]
fn read_meta_struct(buf: &mut Bytes) -> Result<HashMap<BytesString, Vec<u8>>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    fn read_meta_struct_pair(buf: &mut Bytes) -> Result<(BytesString, Vec<u8>), DecodeError> {
        let key = read_string_bytes(buf)?;
        let array_len = decode::read_array_len(unsafe { buf.as_mut_slice() }).map_err(|_| {
            DecodeError::InvalidFormat("Unable to read array len for meta_struct".to_owned())
        })?;

        let mut v = Vec::with_capacity(array_len as usize);

        for _ in 0..array_len {
            let value = read_number_bytes(buf)?;
            v.push(value);
        }
        Ok((key, v))
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;
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
/// * `buf` - A reference to the Bytes containing the encoded map data.
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
#[inline]
fn read_map<K, V, F>(
    len: usize,
    buf: &mut Bytes,
    read_pair: F,
) -> Result<HashMap<K, V>, DecodeError>
where
    K: std::hash::Hash + Eq,
    F: Fn(&mut Bytes) -> Result<(K, V), DecodeError>,
{
    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let (k, v) = read_pair(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

#[inline]
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

/// When you want to "peek" if the next value is a null marker, and only advance the buffer if it is
/// null and return the default value. If it is not null, you can continue to decode as expected.
#[inline]
fn handle_null_marker<T, F>(buf: &mut Bytes, default: F) -> Option<T>
where
    F: FnOnce() -> T,
{
    let slice = unsafe { buf.as_mut_slice() };

    if slice.first() == Some(NULL_MARKER) {
        *slice = &slice[1..];
        Some(default())
    } else {
        None
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
    use tinybytes::BytesString;

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
    fn test_empty_array() {
        let encoded_data = vec![0x90];
        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);
        let (_decoded_traces, decoded_size) = from_slice(bytes).expect("Decoding failed");

        assert_eq!(0, decoded_size);
    }

    #[test]
    fn test_decoder_size() {
        let span = Span {
            name: BytesString::from_slice("span_name".as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let expected_size = encoded_data.len() - 1; // rmp_serde adds additional 0 byte
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (_decoded_traces, decoded_size) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(expected_size, decoded_size);
    }

    #[test]
    fn test_decoder_read_string_success() {
        let expected_string = "test-service-name";
        let span = Span {
            name: BytesString::from_slice(expected_string.as_ref()).unwrap(),
            ..Default::default()
        };
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(expected_string, decoded_span.name.as_str());
    }

    #[test]
    fn test_decoder_read_null_string_success() {
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["name"] = json!(null);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!("", decoded_span.name.as_str());
    }

    #[test]
    fn test_decoder_read_number_success() {
        let span = create_test_json_span(1, 2, 0, 0);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(1, decoded_span.trace_id);
    }

    #[test]
    fn test_decoder_read_null_number_success() {
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["trace_id"] = json!(null);
        let mut encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        encoded_data.extend_from_slice(&[0, 0, 0, 0]); // some garbage, to be ignored
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];
        assert_eq!(0, decoded_span.trace_id);
    }

    #[test]
    fn test_decoder_meta_struct_null_map_success() {
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta_struct"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert!(decoded_span.meta_struct.is_empty());
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
        let (decoded_traces, _) =
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
        let (decoded_traces, _) =
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
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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
                    format!("key {}", i).to_owned(),
                    format!("value {}", i).to_owned(),
                )
            })
            .collect();

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["meta"] = json!(expected_meta.clone());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["metrics"] = json!(expected_metrics.clone());
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
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
        let (decoded_traces, _) =
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
    fn test_decoder_metrics_null_success() {
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["metrics"] = json!(null);
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

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

        let mut span = create_test_json_span(1, 2, 0, 0);
        span["span_links"] = json!([expected_span_link]);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
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
        let mut span = create_test_json_span(1, 2, 0, 0);
        span["span_links"] = json!(null);

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let (decoded_traces, _) =
            from_slice(tinybytes::Bytes::from(encoded_data)).expect("Decoding failed");

        assert_eq!(1, decoded_traces.len());
        assert_eq!(1, decoded_traces[0].len());
        let decoded_span = &decoded_traces[0][0];

        assert!(decoded_span.span_links.is_empty());
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
        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);

        let result = from_slice(bytes);
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
            name: unsafe { BytesString::from_bytes_unchecked(invalid_str_as_bytes) },
            ..Default::default()
        };
        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span]]).unwrap();
        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);

        let result = from_slice(bytes);
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
        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);

        let result = from_slice(bytes);

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

        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);

        let result = from_slice(bytes);

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
        let encoded_data =
            unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(encoded_data.as_ref()) };
        let bytes = tinybytes::Bytes::from_static(encoded_data);

        let result = from_slice(bytes);

        assert_eq!(
            Err(DecodeError::InvalidType(
                "Type mismatch at marker FixPos(1)".to_owned()
            )),
            result
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
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
