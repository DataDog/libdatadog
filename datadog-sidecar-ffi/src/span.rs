// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::span::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{c_char, CString};
use tinybytes::{Bytes, BytesString};

fn convert_char_slice_to_bytes_string(slice: CharSlice) -> BytesString {
    // TODO: Strip the invalid bytes in the tracer instead
    unsafe {
        match String::from_utf8_lossy(slice.as_bytes()) {
            Cow::Owned(s) => s.into(),
            Cow::Borrowed(_) => {
                BytesString::from_bytes_unchecked(Bytes::from_underlying(slice.as_bytes().to_vec()))
            }
        }
    }
}

#[inline]
fn set_string_field(field: &mut BytesString, slice: CharSlice) {
    if slice.is_empty() {
        return;
    }
    *field = convert_char_slice_to_bytes_string(slice);
}

#[inline]
fn get_string_field(field: &BytesString) -> CharSlice<'static> {
    let string = field.as_str();
    unsafe { CharSlice::from_raw_parts(string.as_ptr().cast(), string.len()) }
}

#[inline]
fn insert_hashmap<V>(map: &mut HashMap<BytesString, V>, key: CharSlice, value: V) {
    if key.is_empty() {
        return;
    }
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
    map.insert(bytes_str_key, value);
}

#[inline]
fn remove_hashmap<V>(map: &mut HashMap<BytesString, V>, key: CharSlice) {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
    map.remove(&bytes_str_key);
}

#[inline]
fn exists_hashmap<V>(map: &HashMap<BytesString, V>, key: CharSlice) -> bool {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
    map.contains_key(&bytes_str_key)
}

#[allow(clippy::missing_safety_doc)]
unsafe fn get_hashmap_keys<V>(
    map: &HashMap<BytesString, V>,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    let mut keys: Vec<&str> = map.keys().map(|b| b.as_str()).collect();
    keys.sort_unstable();

    let mut slices = Vec::with_capacity(keys.len());
    for key in keys {
        slices.push(CharSlice::from_raw_parts(key.as_ptr().cast(), key.len()));
    }

    *out_count = slices.len();
    Box::into_raw(slices.into_boxed_slice()) as *mut CharSlice<'static>
}

#[allow(clippy::missing_safety_doc)]
unsafe fn new_vector_item<T: Default>(vec: &mut Vec<T>) -> &mut T {
    vec.push(T::default());
    vec.last_mut().unwrap_unchecked()
}

#[no_mangle]
fn set_event_attribute(
    event: &mut SpanEventBytes,
    key: CharSlice,
    new_item: AttributeArrayValueBytes,
) {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);

    // remove any previous
    let previous = event.attributes.remove(&bytes_str_key);

    // merge old + new
    let merged = match previous {
        None => AttributeAnyValueBytes::SingleValue(new_item),
        Some(AttributeAnyValueBytes::SingleValue(x)) => {
            AttributeAnyValueBytes::Array(vec![x, new_item])
        }
        Some(AttributeAnyValueBytes::Array(mut arr)) => {
            arr.push(new_item);
            AttributeAnyValueBytes::Array(arr)
        }
    };

    event.attributes.insert(bytes_str_key, merged);
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
pub extern "C" fn ddog_get_traces_size(traces: &TracesBytes) -> usize {
    traces.len()
}

#[no_mangle]
pub extern "C" fn ddog_get_trace(traces: &mut TracesBytes, index: usize) -> *mut TraceBytes {
    if index >= traces.len() {
        return std::ptr::null_mut();
    }

    unsafe { traces.get_unchecked_mut(index) as *mut TraceBytes }
}

// ------------------ TraceBytes ------------------

#[no_mangle]
pub extern "C" fn ddog_traces_new_trace(traces: &mut TracesBytes) -> &mut TraceBytes {
    unsafe { new_vector_item(traces) }
}

#[no_mangle]
pub extern "C" fn ddog_get_trace_size(trace: &TraceBytes) -> usize {
    trace.len()
}

#[no_mangle]
pub extern "C" fn ddog_get_span(trace: &mut TraceBytes, index: usize) -> *mut SpanBytes {
    if index >= trace.len() {
        return std::ptr::null_mut();
    }

    unsafe { trace.get_unchecked_mut(index) as *mut SpanBytes }
}

// ------------------- SpanBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_trace_new_span(trace: &mut TraceBytes) -> &mut SpanBytes {
    unsafe { new_vector_item(trace) }
}

#[no_mangle]
pub extern "C" fn ddog_span_debug_log(span: &SpanBytes) -> CharSlice<'static> {
    unsafe {
        let debug_str = format!("{:?}", span);
        let len = debug_str.len();
        let cstring = CString::new(debug_str).unwrap_or_default();

        CharSlice::from_raw_parts(cstring.into_raw().cast(), len)
    }
}

#[no_mangle]
pub extern "C" fn ddog_free_charslice(slice: CharSlice<'static>) {
    let (ptr, len) = slice.as_raw_parts();

    if len == 0 || ptr.is_null() {
        return;
    }

    // SAFETY: we assume this pointer came from `CString::into_raw`
    unsafe {
        let owned_ptr = ptr as *mut c_char;
        let _ = CString::from_raw(owned_ptr);
    }
}

#[no_mangle]
pub extern "C" fn ddog_set_span_service(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field(&mut span.service, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_service(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field(&span.service)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_name(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field(&mut span.name, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_name(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field(&span.name)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_resource(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field(&mut span.resource, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_resource(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field(&span.resource)
}

#[no_mangle]
pub extern "C" fn ddog_set_span_type(span: &mut SpanBytes, slice: CharSlice) {
    set_string_field(&mut span.r#type, slice);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_type(span: &mut SpanBytes) -> CharSlice<'static> {
    get_string_field(&span.r#type)
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
    insert_hashmap(
        &mut span.meta,
        key,
        BytesString::from_slice(value.as_bytes()).unwrap_or_default(),
    );
}

#[no_mangle]
pub extern "C" fn ddog_del_span_meta(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap(&mut span.meta, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_meta(span: &mut SpanBytes, key: CharSlice) -> CharSlice<'static> {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
    match span.meta.get(&bytes_str_key) {
        Some(value) => unsafe {
            CharSlice::from_raw_parts(value.as_str().as_ptr().cast(), value.as_str().len())
        },
        None => CharSlice::empty(),
    }
}

#[no_mangle]
pub extern "C" fn ddog_has_span_meta(span: &mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap(&span.meta, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_meta_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    unsafe { get_hashmap_keys(&span.meta, out_count) }
}

#[no_mangle]
pub extern "C" fn ddog_add_span_metrics(span: &mut SpanBytes, key: CharSlice, val: f64) {
    insert_hashmap(&mut span.metrics, key, val);
}

#[no_mangle]
pub extern "C" fn ddog_del_span_metrics(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap(&mut span.metrics, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_metrics(
    span: &mut SpanBytes,
    key: CharSlice,
    result: &mut f64,
) -> bool {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
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
    exists_hashmap(&span.metrics, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_metrics_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    unsafe { get_hashmap_keys(&span.metrics, out_count) }
}

#[no_mangle]
pub extern "C" fn ddog_add_span_meta_struct(span: &mut SpanBytes, key: CharSlice, val: CharSlice) {
    insert_hashmap(
        &mut span.meta_struct,
        key,
        Bytes::copy_from_slice(val.as_bytes()),
    );
}

#[no_mangle]
pub extern "C" fn ddog_del_span_meta_struct(span: &mut SpanBytes, key: CharSlice) {
    remove_hashmap(&mut span.meta_struct, key);
}

#[no_mangle]
pub extern "C" fn ddog_get_span_meta_struct(
    span: &mut SpanBytes,
    key: CharSlice,
) -> CharSlice<'static> {
    let bytes_str_key = convert_char_slice_to_bytes_string(key);
    match span.meta_struct.get(&bytes_str_key) {
        Some(value) => unsafe { CharSlice::from_raw_parts(value.as_ptr().cast(), value.len()) },
        None => CharSlice::empty(),
    }
}

#[no_mangle]
pub extern "C" fn ddog_has_span_meta_struct(span: &mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap(&span.meta_struct, key)
}

#[no_mangle]
pub extern "C" fn ddog_span_meta_struct_get_keys(
    span: &mut SpanBytes,
    out_count: &mut usize,
) -> *mut CharSlice<'static> {
    unsafe { get_hashmap_keys(&span.meta_struct, out_count) }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_free_keys_ptr(keys_ptr: *mut CharSlice<'static>, count: usize) {
    if keys_ptr.is_null() || count == 0 {
        return;
    }

    Vec::from_raw_parts(keys_ptr, count, count);
}

// ------------------- SpanLinkBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_span_new_link(span: &mut SpanBytes) -> &mut SpanLinkBytes {
    unsafe { new_vector_item(&mut span.span_links) }
}

#[no_mangle]
pub extern "C" fn ddog_set_link_tracestate(link: &mut SpanLinkBytes, slice: CharSlice) {
    set_string_field(&mut link.tracestate, slice);
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
    insert_hashmap(
        &mut link.attributes,
        key,
        BytesString::from_slice(val.as_bytes()).unwrap_or_default(),
    );
}

// ------------------- SpanEventBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_span_new_event(span: &mut SpanBytes) -> &mut SpanEventBytes {
    unsafe { new_vector_item(&mut span.span_events) }
}

#[no_mangle]
pub extern "C" fn ddog_set_event_name(event: &mut SpanEventBytes, slice: CharSlice) {
    set_string_field(&mut event.name, slice);
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
    set_event_attribute(
        event,
        key,
        AttributeArrayValueBytes::String(convert_char_slice_to_bytes_string(val)),
    );
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_bool(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: bool,
) {
    set_event_attribute(event, key, AttributeArrayValueBytes::Boolean(val));
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_int(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: i64,
) {
    set_event_attribute(event, key, AttributeArrayValueBytes::Integer(val));
}

#[no_mangle]
pub extern "C" fn ddog_add_event_attributes_float(
    event: &mut SpanEventBytes,
    key: CharSlice,
    val: f64,
) {
    set_event_attribute(event, key, AttributeArrayValueBytes::Double(val));
}

// ------------------- Export Functions -------------------

#[no_mangle]
pub extern "C" fn ddog_serialize_trace_into_c_string(trace: &mut TraceBytes) -> CharSlice<'static> {
    match rmp_serde::encode::to_vec_named(&vec![trace]) {
        Ok(vec) => {
            let boxed_str = vec.into_boxed_slice();
            let boxed_len = boxed_str.len();

            let leaked_ptr = Box::into_raw(boxed_str) as *const c_char;

            unsafe { CharSlice::from_raw_parts(leaked_ptr, boxed_len) }
        }
        Err(_) => CharSlice::empty(),
    }
}
