// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v05::Span;
use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::{
    map::{read_map_len, read_map},
    number::read_number_bytes,
    string::{handle_null_marker, read_string_bytes},
};
use std::collections::HashMap;

const PAYLOAD_LEN: u32 = 2;
const MAX_SPAN_ELEM: u32 = 12;

/// Decodes a slice of bytes into a vector of `TracerPayloadV05` objects.
///
///
///
/// # Arguments
///
/// * `data` - A tinybytes Bytes buffer containing the encoded data. Bytes are expected to be
///   encoded msgpack data containing a list of a list of v05 spans.
///
/// # Returns
///
/// * `Ok(Vec<TracerPayloadV05>)` - A vector of decoded `TracerPayloadV05` objects if successful.
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
/// use datadog_trace_utils::msgpack_decoder::v05::decoder::from_slice;
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

pub fn from_slice(mut data: tinybytes::Bytes) -> Result<Vec<Vec<Span>>, DecodeError> {
    let data_elem = rmp::decode::read_array_len(unsafe { data.as_mut_slice() })
        .map_err(|_| { DecodeError::InvalidFormat("Unable to read payload len".to_string())})?;

    if data_elem > PAYLOAD_LEN {
        return Err(DecodeError::InvalidFormat("Invalid payload size".to_string()));
    }

    let dict = deserialize_dict(&mut data)?;

    let trace_count = rmp::decode::read_array_len(unsafe {data.as_mut_slice()})
        .map_err(|_| { DecodeError::InvalidFormat("Unable to read trace len".to_string())})?;

    let mut traces: Vec<Vec<Span>> = vec![];

    for _ in 0..trace_count {
        let span_count = rmp::decode::read_array_len(unsafe {data.as_mut_slice()})
            .map_err(|_| { DecodeError::InvalidFormat("Unable to read span len".to_string())})?;
        let mut trace: Vec<Span> = vec![];

        for _ in 0..span_count {
            let span = deserialize_span(&mut data, &dict)?;
            trace.push(span);
        }
        traces.push(trace); 
    }
    Ok(traces)
}

fn deserialize_dict(data: &mut tinybytes::Bytes) -> Result<Vec<tinybytes::BytesString>, DecodeError> {
    let dict_len = rmp::decode::read_array_len(unsafe {data.as_mut_slice()})
        .map_err(|_| { DecodeError::InvalidFormat("Unable to read dictionary len".to_string())})?;
   
    let mut dict: Vec<tinybytes::BytesString> = Vec::with_capacity(dict_len as usize);
    for _ in 0..dict_len {
        let str = read_string_bytes(data)?;
        dict.push(str);
    }
    Ok(dict)
}

fn deserialize_span(data: &mut tinybytes::Bytes, dict: &Vec<tinybytes::BytesString>) -> Result<Span, DecodeError> {
    let mut span = Span::default();
    let span_len = rmp::decode::read_array_len(unsafe { data.as_mut_slice() })
        .map_err(|_| { DecodeError::InvalidFormat("Unable to read dictionary len".to_string())})?;

    if span_len > MAX_SPAN_ELEM {
        return Err(DecodeError::InvalidFormat("Invalid number of span fields".to_string()));
    }

    span.service = get_from_dict(data, &dict)?;
    span.name = get_from_dict(data, &dict)?;
    span.resource = get_from_dict(data, &dict)?;
    span.trace_id = read_number_bytes(data)?;
    span.span_id = read_number_bytes(data)?;
    span.parent_id = read_number_bytes(data)?;
    span.start = read_number_bytes(data)?;
    span.duration = read_number_bytes(data)?;
    span.error = read_number_bytes(data)?;
    span.meta = read_indexed_map_to_bytes_strings(data, &dict)?;
    span.metrics = read_metrics(data, &dict)?;
    span.r#type = get_from_dict(data, &dict)?;

    Ok(span)
}

fn get_from_dict(data: &mut tinybytes::Bytes, dict: &Vec<tinybytes::BytesString>) -> Result<tinybytes::BytesString, DecodeError> {
    let index: u32 = read_number_bytes(data)?;
    match dict.get(index as usize) {
        Some(value) => Ok(value.clone()),
        None => Err(DecodeError::InvalidFormat("Unable to locate string in the dictionary".to_string())),
    }
}

fn read_indexed_map_to_bytes_strings(
    buf: &mut tinybytes::Bytes,
    dict: &Vec<tinybytes::BytesString>,
) -> Result<HashMap<tinybytes::BytesString, tinybytes::BytesString>, DecodeError> {
    let len = rmp::decode::read_map_len(unsafe { buf.as_mut_slice() })
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = HashMap::with_capacity(len.try_into().expect("Unable to cast map len to usize"));
    for _ in 0..len {
        let key = get_from_dict(buf, dict)?;
        let value = get_from_dict(buf, dict)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn read_metrics(buf: &mut tinybytes::Bytes, dict: &Vec<tinybytes::BytesString>) -> Result<HashMap<tinybytes::BytesString, f64>, DecodeError> {
    if let Some(empty_map) = handle_null_marker(buf, HashMap::default) {
        return Ok(empty_map);
    }

    let len = read_map_len(unsafe { buf.as_mut_slice() })?;

    let mut map = HashMap::with_capacity(len);
    for _ in 0..len {
        let k = get_from_dict(buf, dict)?;
        let v = read_number_bytes(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn deserialize_dict_test() {
        let dict = vec!["foo", "bar", "baz"];
        let mpack = rmp_serde::to_vec(&dict).unwrap();
        let mut payload = tinybytes::Bytes::from(mpack);
        
        let result = deserialize_dict(&mut payload).unwrap();
        assert_eq!(result, dict);
    }

    #[test]
    fn from_slice_invalid_size_test() {
        let mpack = rmp_serde::to_vec::<Vec<Vec<u8>>>(&vec![vec![], vec![], vec![]]).unwrap();
        let payload = tinybytes::Bytes::from(mpack);
        assert!(from_slice(payload).is_err());
    }

    #[test]
    fn from_slice_test() {
        let data: (
            Vec<String>,
            Vec<
                Vec<(
                    u8,
                    u8,
                    u8,
                    u64,
                    u64,
                    u64,
                    i64,
                    i64,
                    i32,
                    HashMap<u8, u8>,
                    HashMap<u8, f64>,
                    u8,
                )>,
            >,
        ) = (
            vec![
                "baggage".to_string(),
                "item".to_string(),
                "elasticsearch.version".to_string(),
                "7.0".to_string(),
                "my-name".to_string(),
                "X".to_string(),
                "my-service".to_string(),
                "my-resource".to_string(),
                "_dd.sampling_rate_whatever".to_string(),
                "value whatever".to_string(),
                "sql".to_string(),
            ],
            vec![vec![(
                6,
                4,
                7,
                1,
                2,
                3,
                123,
                456,
                1,
                HashMap::from([(8, 9), (0, 1), (2, 3)]),
                HashMap::from([(5, 1.2)]),
                10,
            )]],
        );
        let msgpack = rmp_serde::to_vec(&data).unwrap();
        let result = from_slice(tinybytes::Bytes::from(msgpack)).unwrap();

        let span = &result[0][0];
        assert_eq!(span.service.as_str(), "my-service");
        assert_eq!(span.name.as_str(), "my-name");
        assert_eq!(span.resource.as_str(), "my-resource");
        assert_eq!(span.trace_id, 1);
        assert_eq!(span.span_id, 2);
        assert_eq!(span.parent_id, 3);
        assert_eq!(span.start, 123);
        assert_eq!(span.duration, 456);
        assert_eq!(span.error, 1);
        assert_eq!(span.meta.len(), 3);
        assert_eq!(span.meta.get("_dd.sampling_rate_whatever").unwrap().as_str(), "value whatever");
        assert_eq!(span.meta.get("baggage").unwrap().as_str(), "item");
        assert_eq!(span.meta.get("elasticsearch.version").unwrap().as_str(), "7.0");
        assert_eq!(span.metrics.len(), 1);
        assert_eq!(*span.metrics.get("X").unwrap(), 1.2 as f64);
        assert_eq!(span.r#type.as_str(), "sql");
    }
}
