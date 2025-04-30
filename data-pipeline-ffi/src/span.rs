// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::span::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use std::ffi::{c_char, CString};
use tinybytes::{Bytes, BytesString};

// ---------------- Macros -------------------

// Set a BytesString field of the given pointer.
macro_rules! set_string_field {
    ($ref:expr, $slice:expr) => {{
        if $slice.is_empty() {
            return;
        }

        $ref = BytesString::from_slice($slice.as_bytes()).unwrap_or_default();
    }};
}

// Get the ByteString field of the given pointer.
macro_rules! get_string_field {
    ($ref:expr) => {{
        let string = $ref.as_str();
        let c_string: CString = CString::new(string).unwrap_or_default();
        let raw: *mut c_char = c_string.into_raw();
        unsafe { CharSlice::from_raw_parts(raw as *const c_char, string.len()) }
    }};
}

// Insert an element in the given hashmap field.
macro_rules! insert_hashmap {
    ($ref:expr, $key:expr, $value:expr) => {{
        if $key.is_empty() {
            return;
        }

        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        $ref.insert(bytes_str_key, $value);
    }};
}

macro_rules! remove_hashmap {
    ($ref:expr, $key:expr) => {{
        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        $ref.remove(&bytes_str_key);
    }};
}

macro_rules! exists_hashmap {
    ($ref:expr, $key:expr) => {{
        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        $ref.contains_key(&bytes_str_key)
    }};
}

macro_rules! get_keys_hashmap {
    ($ref:expr, $out_count:expr) => {{
        unsafe {
            let mut key_strs: Vec<&str> = $ref.keys().map(|k| k.as_str()).collect();

            key_strs.sort_unstable();

            let mut slices = Vec::with_capacity(key_strs.len());
            for key in key_strs {
                let c_string: CString = CString::new(key).unwrap();
                let raw: *mut c_char = c_string.into_raw();
                slices.push(CharSlice::from_raw_parts(raw as *const c_char, key.len()));
            }

            let slice_box = slices.into_boxed_slice();
            *$out_count = slice_box.len();

            Box::into_raw(slice_box) as *mut CharSlice<'static>
        }
    }};
}

macro_rules! new_vector_item {
    ($ref:expr, $item_type:ty) => {{
        $ref.push(<$item_type>::default());
        unsafe { $ref.last_mut().unwrap_unchecked() }
    }};
}

macro_rules! set_event_attribute {
    ($event:expr, $key:expr, $new_item:expr) => {{
        let key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        let value = $new_item;

        // Remove previous value if it exists
        let previous = $event.attributes.remove(&key);

        // Merge the previous value with the new one
        let merged = match previous {
            None => AttributeAnyValueBytes::SingleValue(value),
            Some(AttributeAnyValueBytes::SingleValue(x)) => {
                AttributeAnyValueBytes::Array(vec![x, value])
            }
            Some(AttributeAnyValueBytes::Array(mut array)) => {
                array.push(value);
                AttributeAnyValueBytes::Array(array)
            }
        };

        // Insert the merged value back into the map
        $event.attributes.insert(key, merged);
    }};
}

// ------------------ TracesBytes ------------------

pub type TraceBytes = Vec<SpanBytes>;
pub type TracesBytes = Vec<TraceBytes>;

#[no_mangle]
pub extern "C" fn ddog_get_traces() -> Box<TracesBytes> {
    Box::default()
}

#[no_mangle]
pub extern "C" fn ddog_free_traces(_traces: Box<TracesBytes>) {}

#[no_mangle]
pub extern "C" fn ddog_get_traces_size(traces: &mut TracesBytes) -> usize {
    traces.len()
}

#[no_mangle]
pub extern "C" fn ddog_get_trace(traces: &mut TracesBytes, index: usize) -> Box<TraceBytes> {
    if index >= traces.len() {
        return Box::default();
    }

    unsafe { Box::from_raw(traces.get_unchecked_mut(index) as *mut TraceBytes) }
}

// ------------------ TraceBytes ------------------

#[no_mangle]
pub extern "C" fn ddog_traces_new_trace(traces: &mut TracesBytes) -> &mut TraceBytes {
    new_vector_item!(traces, TraceBytes)
}

#[no_mangle]
pub extern "C" fn ddog_get_trace_size(trace: &mut TraceBytes) -> usize {
    trace.len()
}

#[no_mangle]
pub extern "C" fn ddog_get_span(trace: &mut TraceBytes, index: usize) -> Box<SpanBytes> {
    if index >= trace.len() {
        return Box::default();
    }

    unsafe { Box::from_raw(trace.get_unchecked_mut(index) as *mut SpanBytes) }
}

// ------------------- SpanBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_trace_new_span(trace: &mut TraceBytes) -> &mut SpanBytes {
    new_vector_item!(trace, SpanBytes)
}

#[no_mangle]
pub extern "C" fn ddog_span_debug_log(span: &SpanBytes) -> CharSlice<'static> {
    unsafe {
        let debug_str = format!("{:?}", span);
        let cstring = CString::new(debug_str).unwrap_or_default();
        let len = cstring.to_bytes().len();

        CharSlice::from_raw_parts(cstring.into_raw().cast(), len)
    }
}

#[no_mangle]
pub extern "C" fn ddog_free_charslice(slice: CharSlice<'static>) {
    let slice_ptr = slice.as_ptr() as *mut c_char;
    if slice_ptr.is_null() {
        return;
    }

    unsafe {
        let _ = CString::from_raw(slice_ptr as *mut c_char);
    }
}

#[no_mangle]
pub extern "C" fn ddog_set_span_service(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field!(span.service, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_service(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(span.service)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_name(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field!(span.name, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_name(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(span.name)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_resource(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field!(span.resource, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_resource(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(span.resource)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_type(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field!(span.r#type, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_type(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(span.r#type)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_trace_id(span: &mut SpanBytes, value: u64) {
    span.trace_id = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_trace_id(span: &mut SpanBytes) -> u64 {
    span.trace_id
}

#[no_mangle]
pub extern "C" fn ddog_set_span_id(span: &mut SpanBytes, value: u64) {
    span.span_id = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_id(span: &mut SpanBytes) -> u64 {
    span.span_id
}

#[no_mangle]
pub extern "C" fn ddog_set_span_parent_id(span: &mut SpanBytes, value: u64) {
    span.parent_id = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_parent_id(span: &mut SpanBytes) -> u64 {
    span.parent_id
}

#[no_mangle]
pub extern "C" fn ddog_set_span_start(span: &mut SpanBytes, value: i64) {
    span.start = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_start(span: &mut SpanBytes) -> i64 {
    span.start
}

#[no_mangle]
pub extern "C" fn ddog_set_span_duration(span: &mut SpanBytes, value: i64) {
    span.duration = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_duration(span: &mut SpanBytes) -> i64 {
    span.duration
}

#[no_mangle]
pub extern "C" fn ddog_set_span_error(span: &mut SpanBytes, value: i32) {
    span.error = value;
}

#[no_mangle]
pub extern "C" fn ddog_get_span_error(span: &mut SpanBytes) -> i32 {
    span.error
}

#[no_mangle]
pub extern "C" fn ddog_add_span_meta(span: &mut SpanBytes, key: CharSlice, value: CharSlice) {
    insert_hashmap!(
        span.meta,
        key,
        BytesString::from_slice(value.as_bytes()).unwrap_or_default()
    );
}

#[no_mangle]
pub extern "C" fn ddog_del_span_meta(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap!(span.meta, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_meta(span: &mut SpanBytes, key: CharSlice) -> CharSlice<'static> {
    let bytes_str_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();
    match span.meta.get(&bytes_str_key) {
        Some(value) => unsafe {
            let string = value.as_str();
            let c_string: CString = CString::new(string).unwrap_or_default();
            let raw: *mut c_char = c_string.into_raw();
            CharSlice::from_raw_parts(raw as *const c_char, string.len())
        },
        None => CharSlice::empty(),
    }
}

#[no_mangle]
pub extern "C" fn ddog_has_span_meta(span: &mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap!(span.meta, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_meta_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    get_keys_hashmap!(span.meta, out_count)
}

#[no_mangle]
pub extern "C" fn ddog_add_span_metrics(span: &mut SpanBytes, key: CharSlice, val: f64) {
    insert_hashmap!(span.metrics, key, val);
}

#[no_mangle]
pub extern "C" fn ddog_del_span_metrics(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap!(span.metrics, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_metrics(
    span: &mut SpanBytes,
    key: CharSlice,
    result: &mut f64,
) -> bool {
    let bytes_str_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();
    match span.metrics.get(&bytes_str_key) {
        Some(&value) => {
            *result = value;
            true
        }
        None => false,
    }
}

#[no_mangle]
pub extern "C" fn ddog_has_span_metrics(span: &mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap!(span.metrics, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_metrics_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    get_keys_hashmap!(span.metrics, out_count)
}

#[no_mangle]
pub extern "C" fn ddog_add_span_meta_struct(span: &mut SpanBytes, key: CharSlice, val: CharSlice) {
    insert_hashmap!(
        span.meta_struct,
        key,
        Bytes::copy_from_slice(val.as_bytes())
    );
}

#[no_mangle]
pub extern "C" fn ddog_del_span_meta_struct(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap!(span.meta_struct, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_meta_struct(
    span: &mut SpanBytes,
    key: CharSlice,
) -> CharSlice<'static> {
    let bytes_str_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();
    match span.meta_struct.get(&bytes_str_key) {
        Some(value) => unsafe { CharSlice::from_raw_parts(value.as_ptr().cast(), value.len()) },
        None => CharSlice::empty(),
    }
}

#[no_mangle]
pub extern "C" fn ddog_has_span_meta_struct(span: &mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap!(span.meta_struct, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_meta_struct_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    get_keys_hashmap!(span.meta_struct, out_count)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_free_keys_ptr(keys_ptr: *mut CharSlice<'static>, count: usize) {
    if keys_ptr.is_null() || count == 0 {
        return;
    }

    let slice: &[CharSlice<'static>] = std::slice::from_raw_parts(keys_ptr, count);

    for cs in slice {
        let cs_ptr = cs.as_ptr() as *mut c_char;
        if !cs_ptr.is_null() {
            let _ = CString::from_raw(cs_ptr);
        }
    }

    let _ = Vec::from_raw_parts(keys_ptr, count, count);
}

// ------------------- SpanLinkBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_span_new_link(span: &mut SpanBytes) -> &mut SpanLinkBytes {
    new_vector_item!(span.span_links, SpanLinkBytes)
}

#[no_mangle]
pub extern "C" fn ddog_set_link_tracestate(link: &mut SpanLinkBytes, slice: CharSlice) {
    set_string_field!(link.tracestate, slice);
}

#[no_mangle]
pub extern "C" fn ddog_set_link_trace_id(link: &mut SpanLinkBytes, value: u64) {
    link.trace_id = value;
}

#[no_mangle]
pub extern "C" fn ddog_set_link_trace_id_high(link: &mut SpanLinkBytes, value: u64) {
    link.trace_id_high = value;
}

#[no_mangle]
pub extern "C" fn ddog_set_link_span_id(link: &mut SpanLinkBytes, value: u64) {
    link.span_id = value;
}

#[no_mangle]
pub extern "C" fn ddog_set_link_flags(link: &mut SpanLinkBytes, value: u64) {
    link.flags = value;
}

#[no_mangle]
pub extern "C" fn ddog_add_link_attributes(
    link: &mut SpanLinkBytes,
    key: CharSlice,
    val: CharSlice,
) {
    insert_hashmap!(
        link.attributes,
        key,
        BytesString::from_slice(val.as_bytes()).unwrap_or_default()
    );
}

// ------------------- SpanEventBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_span_new_event(span: &mut SpanBytes) -> &mut SpanEventBytes {
    new_vector_item!(span.span_events, SpanEventBytes)
}

#[no_mangle]
pub extern "C" fn ddog_set_event_name(event: &mut SpanEventBytes, slice: CharSlice) {
    set_string_field!(event.name, slice);
}

#[no_mangle]
pub extern "C" fn ddog_set_event_time(event: &mut SpanEventBytes, val: u64) {
    event.time_unix_nano = val;
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_str(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: CharSlice,
) {
    set_event_attribute!(
        event,
        key,
        AttributeArrayValueBytes::String(
            BytesString::from_slice(val.as_bytes()).unwrap_or_default()
        )
    );
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_bool(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: bool,
) {
    set_event_attribute!(event, key, AttributeArrayValueBytes::Boolean(val));
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_int(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: i64,
) {
    set_event_attribute!(event, key, AttributeArrayValueBytes::Integer(val));
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_float(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: f64,
) {
    set_event_attribute!(event, key, AttributeArrayValueBytes::Double(val));
}

// ------------------- Export Functions -------------------

#[no_mangle]
pub extern "C" fn ddog_serialize_trace_into_c_string(trace: &mut TraceBytes) -> CharSlice<'static> {
    match rmp_serde::encode::to_vec_named(&vec![trace]) {
        Ok(vec) => {
            let string = vec.into_boxed_slice();
            let len = string.len();
            let c_string: CString = CString::new(string).unwrap_or_default();
            let raw: *mut c_char = c_string.into_raw();
            unsafe { CharSlice::from_raw_parts(raw as *const c_char, len) }
        }
        Err(_) => CharSlice::empty(),
    }
}

// ------------------- Tests -------------------

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_utils::span::{
        AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
    };
    use std::collections::HashMap;
    use std::string::String;

    fn get_bytes_str(value: &'static str) -> BytesString {
        From::from(value)
    }
    fn get_bytes(value: &'static str) -> Bytes {
        From::from(String::from(value))
    }

    #[test]
    fn test_empty_span() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);

        let default_span = SpanBytes::default();

        assert_eq!(*span, default_span);

        ddog_free_traces(traces);
    }

    #[test]
    fn test_empty_event() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);
        let event = ddog_span_new_event(span);

        let default_event = SpanEventBytes::default();
        assert_eq!(*event, default_event);
        assert_eq!(span.span_events.len(), 1);

        ddog_free_traces(traces);
    }

    #[test]
    fn test_empty_link() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);
        let link = ddog_span_new_link(span);

        let expected_link = SpanLinkBytes::default();
        assert_eq!(*link, expected_link);
        assert_eq!(span.span_links.len(), 1);

        ddog_free_traces(traces);
    }

    #[test]
    fn test_full_link() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);
        let link = ddog_span_new_link(span);

        ddog_set_link_trace_id(link, 1);
        ddog_set_link_trace_id_high(link, 2);
        ddog_set_link_span_id(link, 3);
        ddog_set_link_flags(link, 4);
        ddog_set_link_tracestate(link, CharSlice::from("tracestate"));
        ddog_add_link_attributes(link, CharSlice::from("attribute"), CharSlice::from("value"));

        let expected_link = SpanLinkBytes {
            trace_id: 1,
            trace_id_high: 2,
            span_id: 3,
            attributes: HashMap::from([(get_bytes_str("attribute"), get_bytes_str("value"))]),
            tracestate: get_bytes_str("tracestate"),
            flags: 4,
        };
        assert_eq!(*link, expected_link);

        assert_eq!(span.span_links.len(), 1);
        assert_eq!(span.span_links[0], expected_link);

        ddog_free_traces(traces);
    }

    #[test]
    fn test_full_event() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);
        let event = ddog_span_new_event(span);

        ddog_set_event_time(event, 1);
        ddog_set_event_name(event, CharSlice::from("name"));
        ddog_add_event_attributes_str(
            event,
            CharSlice::from("str_attribute"),
            CharSlice::from("value"),
        );
        ddog_add_event_attributes_bool(event, CharSlice::from("bool_attribute"), false);
        ddog_add_event_attributes_int(event, CharSlice::from("int_attribute"), 1);
        ddog_add_event_attributes_float(event, CharSlice::from("array_attribute"), 2.0);
        ddog_add_event_attributes_str(
            event,
            CharSlice::from("array_attribute"),
            CharSlice::from("other_value"),
        );

        let expected_event = SpanEventBytes {
            time_unix_nano: 1,
            name: get_bytes_str("name"),
            attributes: HashMap::from([
                (
                    get_bytes_str("str_attribute"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::String(
                        get_bytes_str("value"),
                    )),
                ),
                (
                    get_bytes_str("bool_attribute"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Boolean(false)),
                ),
                (
                    get_bytes_str("int_attribute"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Integer(1)),
                ),
                (
                    get_bytes_str("array_attribute"),
                    AttributeAnyValueBytes::Array(vec![
                        AttributeArrayValueBytes::Double(2.0),
                        AttributeArrayValueBytes::String(get_bytes_str("other_value")),
                    ]),
                ),
            ]),
        };
        assert_eq!(*event, expected_event);

        assert_eq!(span.span_events.len(), 1);
        assert_eq!(span.span_events[0], expected_event);

        ddog_free_traces(traces);
    }

    #[test]
    fn test_full_span() {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(traces.as_mut());
        let span = ddog_trace_new_span(trace);
        let link = ddog_span_new_link(span);

        ddog_set_link_trace_id(link, 10);
        ddog_set_link_span_id(link, 20);
        ddog_set_link_flags(link, 30);

        let event = ddog_span_new_event(span);

        ddog_set_event_time(event, 123456);
        ddog_set_event_name(event, CharSlice::from("event_name"));

        ddog_set_span_service(span, CharSlice::from("service"));
        ddog_set_span_name(span, CharSlice::from("operation"));
        ddog_set_span_resource(span, CharSlice::from("resource"));
        ddog_set_span_type(span, CharSlice::from("type"));
        ddog_set_span_trace_id(span, 1);
        ddog_set_span_id(span, 2);
        ddog_set_span_parent_id(span, 3);
        ddog_set_span_start(span, 4);
        ddog_set_span_duration(span, 5);
        ddog_set_span_error(span, 6);
        ddog_add_span_meta(
            span,
            CharSlice::from("meta_key"),
            CharSlice::from("meta_value"),
        );
        ddog_add_span_metrics(span, CharSlice::from("metric_key"), 1.0);
        ddog_add_span_meta_struct(
            span,
            CharSlice::from("meta_struct_key"),
            CharSlice::from("meta_struct_value"),
        );

        let expected_span = SpanBytes {
            service: get_bytes_str("service"),
            name: get_bytes_str("operation"),
            resource: get_bytes_str("resource"),
            r#type: get_bytes_str("type"),
            trace_id: 1,
            span_id: 2,
            parent_id: 3,
            start: 4,
            duration: 5,
            error: 6,
            meta: HashMap::from([(get_bytes_str("meta_key"), get_bytes_str("meta_value"))]),
            metrics: HashMap::from([(get_bytes_str("metric_key"), 1.0)]),
            meta_struct: HashMap::from([(
                get_bytes_str("meta_struct_key"),
                get_bytes("meta_struct_value"),
            )]),
            span_links: vec![SpanLinkBytes {
                trace_id: 10,
                span_id: 20,
                flags: 30,
                ..Default::default()
            }],
            span_events: vec![SpanEventBytes {
                time_unix_nano: 123456,
                name: get_bytes_str("event_name"),
                attributes: HashMap::new(),
            }],
        };

        assert_eq!(*span, expected_span);

        ddog_free_traces(traces);
    }
}
