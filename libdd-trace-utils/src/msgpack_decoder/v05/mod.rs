// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::msgpack_decoder::decode::error::DecodeError;
use crate::msgpack_decoder::decode::{
    buffer::Buffer, map::read_map_len, number::read_number, string::handle_null_marker,
};
use crate::span::DeserializableTraceData;
use crate::span::v1::from_v04::trace_chunk_from_v04;
use crate::span::v1::{TraceChunkBytes, TraceChunkSlice};
use crate::span::v04::{Span, SpanBytes, SpanSlice};
use crate::span::vec_map::VecMap;

const PAYLOAD_LEN: u32 = 2;
const SPAN_ELEM_COUNT: u32 = 12;

/// Decodes a Bytes buffer into a `Vec<Vec<SpanBytes>>` object, also represented as a vector of
/// `TracerPayloadV05` objects.
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
/// use libdd_tinybytes;
/// use libdd_trace_utils::msgpack_decoder::v05::from_bytes;
/// use rmp_serde::to_vec;
/// use std::collections::HashMap;
///
/// let data = (
///     vec!["".to_string()],
///     vec![vec![(
///         0,
///         0,
///         0,
///         1,
///         2,
///         3,
///         4,
///         5,
///         6,
///         HashMap::<u32, u32>::new(),
///         HashMap::<u32, f64>::new(),
///         0,
///     )]],
/// );
/// let encoded_data = to_vec(&data).unwrap();
/// let encoded_data_as_tinybytes = libdd_tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_bytes(encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("", decoded_span.name.as_str());
/// ```
pub fn from_bytes(
    data: libdd_tinybytes::Bytes,
) -> Result<(Vec<Vec<SpanBytes>>, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

/// Decodes a slice of bytes into a `Vec<Vec<SpanSlice>>` object.
/// The resulting spans have the same lifetime as the initial buffer.
///
/// # Arguments
///
/// * `data` - A slice of bytes containing the encoded data. Bytes are expected to be encoded
///   msgpack data containing a list of a list of v05 spans.
///
/// # Returns
///
/// * `Ok(Vec<Vec<SpanSlice>>)` - A vector of decoded `Vec<SpanSlice>` objects if successful.
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
/// use libdd_tinybytes;
/// use libdd_trace_utils::msgpack_decoder::v05::from_slice;
/// use rmp_serde::to_vec;
/// use std::collections::HashMap;
///
/// let data = (
///     vec!["".to_string()],
///     vec![vec![(
///         0,
///         0,
///         0,
///         1,
///         2,
///         3,
///         4,
///         5,
///         6,
///         HashMap::<u32, u32>::new(),
///         HashMap::<u32, f64>::new(),
///         0,
///     )]],
/// );
/// let encoded_data = to_vec(&data).unwrap();
/// let encoded_data_as_tinybytes = libdd_tinybytes::Bytes::from(encoded_data);
/// let (decoded_traces, _payload_size) =
///     from_slice(&encoded_data_as_tinybytes).expect("Decoding failed");
///
/// assert_eq!(1, decoded_traces.len());
/// assert_eq!(1, decoded_traces[0].len());
/// let decoded_span = &decoded_traces[0][0];
/// assert_eq!("", decoded_span.name);
/// ```
pub fn from_slice(data: &[u8]) -> Result<(Vec<Vec<SpanSlice<'_>>>, usize), DecodeError> {
    from_buffer(&mut Buffer::new(data))
}

#[allow(clippy::type_complexity)]
fn from_buffer<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
) -> Result<(Vec<Vec<Span<T>>>, usize), DecodeError>
where
    T::Text: Clone,
{
    decode_traces(data, |trace| trace)
}

/// Decodes a Bytes buffer of v0.5 msgpack traces straight into V1 [`TraceChunkBytes`].
///
/// Each trace is converted as soon as it is decoded (see
/// [`crate::span::v1::from_v04::trace_chunk_from_v04`] for the mapping, v0.5 spans carrying the
/// same fields as v0.4 ones), so the spans of the whole payload are never held at once in their
/// v0.5 shape.
pub fn from_bytes_into_v1(
    data: libdd_tinybytes::Bytes,
) -> Result<(Vec<TraceChunkBytes>, usize), DecodeError> {
    decode_traces(&mut Buffer::new(data), trace_chunk_from_v04)
}

/// Decodes a slice of v0.5 msgpack traces straight into V1 [`TraceChunkSlice`]s borrowing from
/// `data`. See [`from_bytes_into_v1`].
pub fn from_slice_into_v1(data: &[u8]) -> Result<(Vec<TraceChunkSlice<'_>>, usize), DecodeError> {
    decode_traces(&mut Buffer::new(data), trace_chunk_from_v04)
}

/// Decodes the v0.5 `[dict, traces]` payload, mapping each trace through `map_trace` as soon as
/// its spans are decoded.
fn decode_traces<T: DeserializableTraceData, C>(
    data: &mut Buffer<T>,
    mut map_trace: impl FnMut(Vec<Span<T>>) -> C,
) -> Result<(Vec<C>, usize), DecodeError>
where
    T::Text: Clone,
{
    let data_elem = rmp::decode::read_array_len(data.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read payload len".to_string()))?;

    if data_elem != PAYLOAD_LEN {
        return Err(DecodeError::InvalidFormat(
            "Invalid payload size".to_string(),
        ));
    }

    let dict = deserialize_dict(data)?;

    let trace_count = rmp::decode::read_array_len(data.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read trace len".to_string()))?;

    let mut traces: Vec<C> = Vec::with_capacity(trace_count as usize);
    let start_len = data.len();

    for _ in 0..trace_count {
        let span_count = rmp::decode::read_array_len(data.as_mut_slice())
            .map_err(|_| DecodeError::InvalidFormat("Unable to read span len".to_string()))?;
        let mut trace: Vec<Span<T>> = Vec::with_capacity(span_count as usize);

        for _ in 0..span_count {
            let span = deserialize_span(data, &dict)?;
            trace.push(span);
        }
        traces.push(map_trace(trace));
    }
    Ok((traces, start_len - data.len()))
}

fn deserialize_dict<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
) -> Result<Vec<T::Text>, DecodeError> {
    let dict_len = rmp::decode::read_array_len(data.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read dictionary len".to_string()))?;

    let mut dict: Vec<T::Text> = Vec::with_capacity(dict_len as usize);
    for _ in 0..dict_len {
        let str = data.read_string()?;
        dict.push(str);
    }
    Ok(dict)
}

fn deserialize_span<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
    dict: &[T::Text],
) -> Result<Span<T>, DecodeError>
where
    T::Text: Clone,
{
    let mut span = Span::default();
    let span_len = rmp::decode::read_array_len(data.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to read dictionary len".to_string()))?;

    if span_len != SPAN_ELEM_COUNT {
        return Err(DecodeError::InvalidFormat(
            "Invalid number of span fields".to_string(),
        ));
    }

    span.service = get_from_dict(data, dict)?;
    span.name = get_from_dict(data, dict)?;
    span.resource = get_from_dict(data, dict)?;
    span.trace_id = read_number::<_, u64>(data)? as u128;
    span.span_id = read_number(data)?;
    span.parent_id = read_number(data)?;
    span.start = read_number(data)?;
    span.duration = read_number(data)?;
    span.error = read_number(data)?;
    span.meta = read_indexed_map_to_bytes_strings(data, dict)?;
    span.metrics = read_metrics(data, dict)?;
    span.r#type = get_from_dict(data, dict)?;

    // Decoded from msgpack maps (unique keys) or empty defaults: no duplicates, so mark deduped to
    // skip the defensive dedup (and its warning) at encoding time. A later mutation re-dirties it.
    span.meta.mark_deduped();
    span.metrics.mark_deduped();
    span.meta_struct.mark_deduped();

    Ok(span)
}

fn get_from_dict<T: DeserializableTraceData>(
    data: &mut Buffer<T>,
    dict: &[T::Text],
) -> Result<T::Text, DecodeError>
where
    T::Text: Clone,
{
    let index: u32 = read_number(data)?;
    match dict.get(index as usize) {
        Some(value) => Ok(value.clone()),
        None => Err(DecodeError::InvalidFormat(
            "Unable to locate string in the dictionary".to_string(),
        )),
    }
}

fn read_indexed_map_to_bytes_strings<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    dict: &[T::Text],
) -> Result<VecMap<T::Text, T::Text>, DecodeError>
where
    T::Text: Clone,
{
    let len = rmp::decode::read_map_len(buf.as_mut_slice())
        .map_err(|_| DecodeError::InvalidFormat("Unable to get map len for str map".to_owned()))?;

    let mut map = VecMap::with_capacity(len.try_into().unwrap_or_default());
    for _ in 0..len {
        let key = get_from_dict(buf, dict)?;
        let value = get_from_dict(buf, dict)?;
        map.insert(key, value);
    }
    Ok(map)
}

fn read_metrics<T: DeserializableTraceData>(
    buf: &mut Buffer<T>,
    dict: &[T::Text],
) -> Result<VecMap<T::Text, f64>, DecodeError>
where
    T::Text: Clone,
{
    if handle_null_marker(buf) {
        return Ok(VecMap::new());
    }

    let len = read_map_len(buf)?;

    let mut map = VecMap::with_capacity(len);
    for _ in 0..len {
        let k = get_from_dict(buf, dict)?;
        let v = read_number(buf)?;
        map.insert(k, v);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SliceData;
    use std::collections::HashMap;

    type V05Span = (
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
    );

    type V05SpanMalformed = (
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
    );

    type V05Payload = (Vec<String>, Vec<Vec<V05Span>>);
    type V05PayloadMalformed = (Vec<String>, Vec<Vec<V05SpanMalformed>>);

    #[test]
    fn test_from_slice_into_v1() {
        let mut span = crate::span::v04::SpanBytes {
            name: libdd_tinybytes::BytesString::from_static("op"),
            trace_id: 3,
            span_id: 1,
            ..Default::default()
        };
        span.meta.insert(
            libdd_tinybytes::BytesString::from_static("span.kind"),
            libdd_tinybytes::BytesString::from_static("server"),
        );
        let crate::tracer_payload::TraceChunks::V05(payload) =
            crate::trace_utils::convert_trace_chunks_v04_to_v05(&[vec![span]]).unwrap()
        else {
            panic!("expected a v0.5 payload");
        };
        let encoded = rmp_serde::to_vec(&payload).unwrap();

        let (chunks, _) = from_slice_into_v1(&encoded).expect("Decoding failed");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].trace_id[15], 3);
        assert_eq!(chunks[0].spans[0].name, "op");
        assert_eq!(
            chunks[0].spans[0].span_kind,
            crate::span::v1::SpanKind::Server
        );

        let (owned, _) = from_bytes_into_v1(libdd_tinybytes::Bytes::from(encoded.clone()))
            .expect("Decoding failed");
        assert_eq!(owned[0].spans[0].name.as_str(), "op");
    }

    #[test]
    fn deserialize_dict_test() {
        let dict = vec!["foo", "bar", "baz"];
        let mpack = rmp_serde::to_vec(&dict).unwrap();
        let mut payload = Buffer::<SliceData>::new(mpack.as_ref());

        let result = deserialize_dict(&mut payload).unwrap();
        assert_eq!(dict, result);
    }

    #[test]
    fn from_bytes_invalid_size_test() {
        // 3 empty array.
        let empty_three: [u8; 3] = [0x93, 0x90, 0x90];
        let payload = unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(&empty_three) };
        let bytes = libdd_tinybytes::Bytes::from_static(payload);
        let result = from_bytes(bytes);

        assert!(result.is_err());
        matches!(result.err().unwrap(), DecodeError::InvalidFormat(_));

        // 1 empty array
        let empty_one: [u8; 2] = [0x91, 0x90];
        let payload = unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(&empty_one) };
        let bytes = libdd_tinybytes::Bytes::from_static(payload);
        let result = from_bytes(bytes);

        assert!(result.is_err());
        matches!(result.err().unwrap(), DecodeError::InvalidFormat(_));
    }

    #[test]
    fn from_bytes_test() {
        let data: V05Payload = (
            vec![
                "".to_string(),
                "item".to_string(),
                "version".to_string(),
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
        let (traces, _) = from_bytes(libdd_tinybytes::Bytes::from(msgpack)).unwrap();

        let span = &traces[0][0];
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
        assert_eq!(
            span.meta
                .get("_dd.sampling_rate_whatever")
                .unwrap()
                .as_str(),
            "value whatever"
        );
        assert_eq!(span.meta.get("").unwrap().as_str(), "item");
        assert_eq!(span.meta.get("version").unwrap().as_str(), "7.0");
        assert_eq!(span.metrics.len(), 1);
        assert_eq!(*span.metrics.get("X").unwrap(), 1.2_f64);
        assert_eq!(span.r#type.as_str(), "sql");
    }

    #[test]
    fn missing_dict_elements_test() {
        let data: V05Payload = (
            vec![
                "".to_string(),
                "item".to_string(),
                "version".to_string(),
                "7.0".to_string(),
                "my-name".to_string(),
                "X".to_string(),
                "my-service".to_string(),
                "my-resource".to_string(),
                "_dd.sampling_rate_whatever".to_string(),
                "value whatever".to_string(),
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
        let payload = rmp_serde::to_vec(&data).unwrap();
        let payload = unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(&payload) };
        let result = from_bytes(libdd_tinybytes::Bytes::from_static(payload));

        assert!(result.is_err());

        // Unable to locate string in the dictionary
        matches!(result.err().unwrap(), DecodeError::InvalidFormat(_));
    }

    #[test]
    fn missing_span_elements_test() {
        let data: V05PayloadMalformed = (
            vec![
                "".to_string(),
                "item".to_string(),
                "version".to_string(),
                "7.0".to_string(),
                "my-name".to_string(),
                "X".to_string(),
                "my-service".to_string(),
                "my-resource".to_string(),
                "_dd.sampling_rate_whatever".to_string(),
                "value whatever".to_string(),
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
            )]],
        );

        let payload = rmp_serde::to_vec(&data).unwrap();
        let payload = unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(&payload) };
        let result = from_bytes(libdd_tinybytes::Bytes::from_static(payload));

        assert!(result.is_err());

        // Invalid number of span fields.
        matches!(result.err().unwrap(), DecodeError::InvalidFormat(_));
    }
}
