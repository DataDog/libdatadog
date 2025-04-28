// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_ipc::platform::{MappedMem, ShmHandle};
use datadog_sidecar_ffi::{
    ddog_alloc_anon_shm_handle, ddog_map_shm, ddog_sidecar_send_trace_v04_bytes,
    ddog_sidecar_send_trace_v04_shm, ddog_unmap_shm, TracerHeaderTags,
};
use datadog_trace_utils::span::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use ddcommon_ffi::{
    ddog_Error_message,
    slice::{AsBytes, CharSlice},
    MaybeError,
};
use std::ffi::c_char;
use std::ffi::c_void;
use std::io::Cursor;
use std::slice;
use tinybytes::{Bytes, BytesString};

use datadog_sidecar::service::{blocking::SidecarTransport, InstanceId};

// ---------------- Macros -------------------

// Set a BytesString field of the given pointer.
macro_rules! set_string_field {
    ($ptr:expr, $slice:expr, $field:ident) => {{
        if $ptr.is_null() || $slice.is_empty() {
            return;
        }

        let object = &mut *$ptr;
        object.$field = BytesString::from_slice($slice.as_bytes()).unwrap_or_default();
    }};
}

// Get the ByteString field of the given pointer.
macro_rules! get_string_field {
    ($ptr:expr, $field:ident) => {{
        if $ptr.is_null() {
            return CharSlice::empty();
        }

        let object = &mut *$ptr;

        CharSlice::from_raw_parts(
            object.$field.as_str().as_ptr().cast(),
            object.$field.as_str().len(),
        )
    }};
}

// Set a numerical field of the given pointer.
macro_rules! set_numeric_field {
    ($ptr:expr, $value:expr, $field:ident) => {{
        if $ptr.is_null() {
            return;
        }
        let object = &mut *$ptr;
        object.$field = $value;
    }};
}

// Get a field from the given pointer.
macro_rules! get_numeric_field {
    ($ptr:expr, $field:ident) => {{
        if $ptr.is_null() {
            return 0;
        }

        let object = &mut *$ptr;

        object.$field
    }};
}

// Insert an element in the given hashmap field.
macro_rules! insert_hashmap {
    ($ptr:expr, $key:expr, $value:expr, $field:ident) => {{
        if $ptr.is_null() {
            return;
        }
        let object = &mut *$ptr;
        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        object.$field.insert(bytes_str_key, $value);
    }};
}

macro_rules! remove_hashmap {
    ($ptr:expr, $key:expr, $field:ident) => {{
        if $ptr.is_null() {
            return;
        }
        let object = &mut *$ptr;
        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        object.$field.remove(&bytes_str_key);
    }};
}

macro_rules! exists_hashmap {
    ($ptr:expr, $key:expr, $field:ident) => {{
        if $ptr.is_null() {
            return false;
        }
        let object = &mut *$ptr;
        let bytes_str_key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        return object.$field.contains_key(&bytes_str_key);
    }};
}

macro_rules! get_keys_hashmap {
    ($span_ptr:expr, $out_count:expr, $field:ident) => {{
        if $span_ptr.is_null() || $out_count.is_null() {
            return std::ptr::null_mut();
        }

        let span = &mut *$span_ptr;

        let mut slices: Vec<CharSlice<'static>> = Vec::with_capacity(span.$field.len());

        for key in span.$field.keys() {
            let value = CharSlice::from_raw_parts(key.as_str().as_ptr().cast(), key.as_str().len());
            slices.push(value);
        }

        let slice_box = slices.into_boxed_slice();
        *$out_count = slice_box.len();

        Box::into_raw(slice_box) as *mut CharSlice<'static>
    }};
}

macro_rules! new_vector_item {
    ($ptr:expr, $field:ident, $item_type:ty) => {{
        if $ptr.is_null() {
            return std::ptr::null_mut();
        }

        let object = unsafe { &mut *$ptr };
        object.$field.push(<$item_type>::default());

        let item = object.$field.last_mut().unwrap();

        item as *mut $item_type
    }};
}

macro_rules! set_event_attribute {
    ($ptr:expr, $key:expr, $new_item:expr) => {{
        if $ptr.is_null() {
            return;
        }

        let event = unsafe { &mut *$ptr };
        let key = BytesString::from_slice($key.as_bytes()).unwrap_or_default();
        let value = $new_item;

        // Remove previous value if it exists
        let previous = event.attributes.remove(&key);

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
        event.attributes.insert(key, merged);
    }};
}

// ------------------ TracesBytes ------------------

pub type TraceBytes = Vec<SpanBytes>;
pub type TracesBytes = Vec<TraceBytes>;

#[no_mangle]
pub extern "C" fn ddog_get_traces() -> *mut TracesBytes {
    Box::into_raw(Box::default())
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_free_traces(ptr: *mut TracesBytes) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_traces_size(ptr: *mut TracesBytes) -> usize {
    if ptr.is_null() {
        return 0;
    }

    let object = &mut *ptr;
    object.len()
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_trace(ptr: *mut TracesBytes, index: usize) -> *mut TraceBytes {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let object = &mut *ptr;
    if index >= object.len() {
        return std::ptr::null_mut();
    }

    &mut object[index] as *mut TraceBytes
}

// ------------------ TracesBytes ------------------

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_traces_new_trace(ptr: *mut TracesBytes) -> *mut TraceBytes {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let object = &mut *ptr;
    object.push(TraceBytes::default());
    object.last_mut().unwrap_unchecked() as *mut TraceBytes
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_trace_new_span(ptr: *mut TraceBytes) -> *mut SpanBytes {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let object = &mut *ptr;
    object.push(SpanBytes::default());
    object.last_mut().unwrap_unchecked() as *mut SpanBytes
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_trace_size(ptr: *mut TraceBytes) -> usize {
    if ptr.is_null() {
        return 0;
    }

    let object = &mut *ptr;
    object.len()
}

// ------------------- SpanBytes -------------------

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_debug_log(ptr: *const SpanBytes) -> CharSlice<'static> {
    if ptr.is_null() {
        return CharSlice::empty();
    }

    let span = &*ptr;
    let debug_str = format!("{:?}", span);

    let boxed_str = debug_str.into_boxed_str();
    let boxed_len = boxed_str.len();

    let leaked_ptr = Box::into_raw(boxed_str) as *const c_char;

    CharSlice::from_raw_parts(leaked_ptr, boxed_len)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_free_charslice(slice: CharSlice<'static>) {
    let data = slice.as_slice();

    if data.is_empty() {
        return;
    }

    let data_ptr = data.as_ptr() as *mut u8;

    // Memory should be valid UTF-8 if it came from ddog_span_debug_log
    drop(Box::from_raw(
        std::str::from_utf8_mut(std::slice::from_raw_parts_mut(data_ptr, data.len()))
            .unwrap_unchecked(),
    ));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_service(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, service);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_service(ptr: *mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(ptr, service)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_name(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, name);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_name(ptr: *mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(ptr, name)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_resource(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, resource);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_resource(ptr: *mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(ptr, resource)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_type(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, r#type);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_type(ptr: *mut SpanBytes) -> CharSlice<'static> {
    get_string_field!(ptr, r#type)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_trace_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, trace_id);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_trace_id(ptr: *mut SpanBytes) -> u64 {
    get_numeric_field!(ptr, trace_id)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_span_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, span_id);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_span_id(ptr: *mut SpanBytes) -> u64 {
    get_numeric_field!(ptr, span_id)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_parent_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, parent_id);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_parent_id(ptr: *mut SpanBytes) -> u64 {
    get_numeric_field!(ptr, parent_id)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_start(ptr: *mut SpanBytes, value: i64) {
    set_numeric_field!(ptr, value, start);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_start(ptr: *mut SpanBytes) -> i64 {
    get_numeric_field!(ptr, start)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_duration(ptr: *mut SpanBytes, value: i64) {
    set_numeric_field!(ptr, value, duration);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_duration(ptr: *mut SpanBytes) -> i64 {
    get_numeric_field!(ptr, duration)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_span_error(ptr: *mut SpanBytes, value: i32) {
    set_numeric_field!(ptr, value, error);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_error(ptr: *mut SpanBytes) -> i32 {
    get_numeric_field!(ptr, error)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_span_meta(ptr: *mut SpanBytes, key: CharSlice, val: CharSlice) {
    insert_hashmap!(
        ptr,
        key,
        BytesString::from_slice(val.as_bytes()).unwrap_or_default(),
        meta
    );
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_del_span_meta(ptr: *mut SpanBytes, key: CharSlice) {
    remove_hashmap!(ptr, key, meta);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_meta(
    ptr: *mut SpanBytes,
    key: CharSlice,
) -> CharSlice<'static> {
    if ptr.is_null() {
        return CharSlice::empty();
    }

    let span = &mut *ptr;

    let bytes_str_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();

    match span.meta.get(&bytes_str_key) {
        Some(value) => {
            CharSlice::from_raw_parts(value.as_str().as_ptr().cast(), value.as_str().len())
        }
        None => CharSlice::empty(),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_has_span_meta(ptr: *mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap!(ptr, key, meta);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_meta_get_keys(
    span_ptr: *mut SpanBytes,
    out_count: *mut usize,
) -> *mut CharSlice<'static> {
    get_keys_hashmap!(span_ptr, out_count, meta)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_span_metrics(ptr: *mut SpanBytes, key: CharSlice, val: f64) {
    insert_hashmap!(ptr, key, val, metrics);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_del_span_metrics(ptr: *mut SpanBytes, key: CharSlice) {
    remove_hashmap!(ptr, key, metrics);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_get_span_metrics(
    ptr: *mut SpanBytes,
    key: CharSlice,
    result: *mut f64,
) -> bool {
    if ptr.is_null() {
        return false;
    }

    let span = &mut *ptr;

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
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_has_span_metrics(ptr: *mut SpanBytes, key: CharSlice) -> bool {
    exists_hashmap!(ptr, key, metrics);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_metrics_get_keys(
    span_ptr: *mut SpanBytes,
    out_count: *mut usize,
) -> *mut CharSlice<'static> {
    get_keys_hashmap!(span_ptr, out_count, metrics)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_span_meta_struct(
    ptr: *mut SpanBytes,
    key: CharSlice,
    val: CharSlice,
) {
    insert_hashmap!(
        ptr,
        key,
        Bytes::copy_from_slice(val.as_bytes()),
        meta_struct
    );
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_free_keys_ptr(keys_ptr: *mut CharSlice<'static>, count: usize) {
    if keys_ptr.is_null() {
        return;
    }

    drop(Box::from_raw(std::slice::from_raw_parts_mut(
        keys_ptr, count,
    )));
}

// ------------------- SpanLinkBytes -------------------

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_new_link(span_ptr: *mut SpanBytes) -> *mut SpanLinkBytes {
    new_vector_item!(span_ptr, span_links, SpanLinkBytes)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_link_tracestate(ptr: *mut SpanLinkBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, tracestate);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_link_trace_id(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, trace_id);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_link_trace_id_high(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, trace_id_high);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_link_span_id(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, span_id);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_link_flags(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, flags);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_link_attributes(
    ptr: *mut SpanLinkBytes,
    key: CharSlice,
    val: CharSlice,
) {
    insert_hashmap!(
        ptr,
        key,
        BytesString::from_slice(val.as_bytes()).unwrap_or_default(),
        attributes
    );
}

// ------------------- SpanEventBytes -------------------

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_span_new_event(span_ptr: *mut SpanBytes) -> *mut SpanEventBytes {
    new_vector_item!(span_ptr, span_events, SpanEventBytes)
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_event_name(ptr: *mut SpanEventBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, name);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_set_event_time(ptr: *mut SpanEventBytes, val: u64) {
    set_numeric_field!(ptr, val, time_unix_nano);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_event_attributes_str(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: CharSlice,
) {
    set_event_attribute!(
        ptr,
        key,
        AttributeArrayValueBytes::String(
            BytesString::from_slice(val.as_bytes()).unwrap_or_default()
        )
    );
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_event_attributes_bool(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: bool,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Boolean(val));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_event_attributes_int(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: i64,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Integer(val));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_add_event_attributes_float(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: f64,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Double(val));
}

// ------------------- Export Functions -------------------

#[repr(C)]
#[derive()]
pub struct SenderParameters {
    pub tracer_headers_tags: TracerHeaderTags<'static>,
    pub transport: Box<SidecarTransport>,
    pub instance_id: *mut InstanceId,
    pub limit: usize,
    pub n_requests: i64,
    pub buffer_size: i64,
    pub url: CharSlice<'static>,
}

unsafe fn check_error(msg: &str, maybe_error: MaybeError) -> bool {
    if maybe_error != MaybeError::None {
        tracing::error!("{}: {}", msg, ddog_Error_message(maybe_error.to_std_ref()));
        return false;
    }
    true
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_serialize_trace_into_c_string(
    trace_ptr: *mut TraceBytes,
) -> CharSlice<'static> {
    if trace_ptr.is_null() {
        return CharSlice::empty();
    }

    let trace = &*trace_ptr;
    match rmp_serde::encode::to_vec_named(trace) {
        Ok(vec) => {
            let boxed_str = vec.into_boxed_slice();
            let boxed_len = boxed_str.len();

            let leaked_ptr = Box::into_raw(boxed_str) as *const c_char;

            CharSlice::from_raw_parts(leaked_ptr, boxed_len)
        }
        Err(_) => CharSlice::empty(),
    }
}

unsafe fn serialize_traces_into_mapped_memory(
    traces_ptr: *const TracesBytes,
    buf_ptr: *mut c_void,
    cap: usize,
) -> usize {
    if traces_ptr.is_null() || buf_ptr.is_null() || cap == 0 {
        return 0;
    }

    // view the raw buffer as &mut [u8]
    let dst = slice::from_raw_parts_mut(buf_ptr.cast::<u8>(), cap);
    let mut cursor = Cursor::new(dst);

    match rmp_serde::encode::write_named(&mut cursor, &*traces_ptr) {
        Ok(()) => cursor.position() as usize,
        Err(_) => 0,
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn ddog_send_traces_to_sidecar(
    traces_ptr: *mut TracesBytes,
    parameters: &mut SenderParameters,
) {
    if traces_ptr.is_null() {
        tracing::error!("Invalid traces pointer");
        return;
    }

    let traces = &*traces_ptr;
    let size: usize = traces.iter().map(|trace| trace.len()).sum();

    if parameters.transport.is_closed() {
        tracing::info!("Skipping flushing traces of size {} as connection to sidecar failed", size);
        return;
    }

    let mut shm: *mut ShmHandle = std::ptr::null_mut();
    let mut mapped_shm: *mut MappedMem<ShmHandle> = std::ptr::null_mut();

    if !check_error(
        "Failed allocating shared memory",
        ddog_alloc_anon_shm_handle(parameters.limit, &mut shm),
    ) {
        return;
    }

    let mut size: usize = 0;
    let mut pointer = std::ptr::null_mut();
    if !check_error(
        "Failed mapping shared memory",
        ddog_map_shm(Box::from_raw(shm), &mut mapped_shm, &mut pointer, &mut size),
    ) {
        return;
    }

    let boxed_mapped_shm = Box::from_raw(mapped_shm);

    let written = serialize_traces_into_mapped_memory(traces, pointer, size);
    ddog_unmap_shm(boxed_mapped_shm);
    if !written == 0 {
        return;
    }

    let mut size_hint = written;
    if parameters.n_requests > 0 {
        size_hint = size_hint.max((parameters.buffer_size / parameters.n_requests + 1) as usize);
    }

    let send_error = ddog_sidecar_send_trace_v04_shm(
        &mut parameters.transport,
        &*parameters.instance_id,
        Box::from_raw(shm),
        size_hint,
        &parameters.tracer_headers_tags,
    );

    loop {
        if send_error != MaybeError::None {
            let mut buffer = vec![0u8; written];
            pointer = buffer.as_mut_ptr() as *mut c_void;
            serialize_traces_into_mapped_memory(traces, pointer, written);

            let retry_error = ddog_sidecar_send_trace_v04_bytes(
                &mut parameters.transport,
                &*parameters.instance_id,
                CharSlice::from_raw_parts(pointer.cast(), written),
                &parameters.tracer_headers_tags,
            );

            if check_error("Failed sending traces to the sidecar", retry_error) {
                tracing::debug!("Failed sending traces via shm to sidecar: {}", ddog_Error_message(send_error.to_std_ref()));
            } else {
                break;
            }
        }

        tracing::info!("Flushing traces of size {} to send-queue for {}", size, parameters.url);
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
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);

            let empty_span = &*span_ptr;
            let default_span = SpanBytes::default();

            assert_eq!(*empty_span, default_span);

            ddog_free_traces(traces_ptr);
        }
    }

    #[test]
    fn test_empty_event() {
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);
            let event_ptr = ddog_span_new_event(span_ptr);
            let event_ref = &*event_ptr;

            let default_event = SpanEventBytes::default();
            assert_eq!(*event_ref, default_event);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_events.len(), 1);

            ddog_free_traces(traces_ptr);
        }
    }

    #[test]
    fn test_empty_link() {
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);
            let link_ptr = ddog_span_new_link(span_ptr);

            let link_ref = &*link_ptr;
            let expected_link = SpanLinkBytes::default();
            assert_eq!(*link_ref, expected_link);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_links.len(), 1);

            ddog_free_traces(traces_ptr);
        }
    }

    #[test]
    fn test_full_link() {
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);
            let link_ptr = ddog_span_new_link(span_ptr);

            ddog_set_link_trace_id(link_ptr, 1);
            ddog_set_link_trace_id_high(link_ptr, 2);
            ddog_set_link_span_id(link_ptr, 3);
            ddog_set_link_flags(link_ptr, 4);
            ddog_set_link_tracestate(link_ptr, CharSlice::from("tracestate"));
            ddog_add_link_attributes(
                link_ptr,
                CharSlice::from("attribute"),
                CharSlice::from("value"),
            );

            let link_ref = &*link_ptr;
            let expected_link = SpanLinkBytes {
                trace_id: 1,
                trace_id_high: 2,
                span_id: 3,
                attributes: HashMap::from([(get_bytes_str("attribute"), get_bytes_str("value"))]),
                tracestate: get_bytes_str("tracestate"),
                flags: 4,
            };
            assert_eq!(*link_ref, expected_link);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_links.len(), 1);
            assert_eq!(span_ref.span_links[0], expected_link);

            ddog_free_traces(traces_ptr);
        }
    }

    #[test]
    fn test_full_event() {
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);
            let event_ptr = ddog_span_new_event(span_ptr);

            ddog_set_event_time(event_ptr, 1);
            ddog_set_event_name(event_ptr, CharSlice::from("name"));
            ddog_add_event_attributes_str(
                event_ptr,
                CharSlice::from("str_attribute"),
                CharSlice::from("value"),
            );
            ddog_add_event_attributes_bool(event_ptr, CharSlice::from("bool_attribute"), false);
            ddog_add_event_attributes_int(event_ptr, CharSlice::from("int_attribute"), 1);
            ddog_add_event_attributes_float(event_ptr, CharSlice::from("array_attribute"), 2.0);
            ddog_add_event_attributes_str(
                event_ptr,
                CharSlice::from("array_attribute"),
                CharSlice::from("other_value"),
            );

            let event_ref = &*event_ptr;
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
                        AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Boolean(
                            false,
                        )),
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
            assert_eq!(*event_ref, expected_event);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_events.len(), 1);
            assert_eq!(span_ref.span_events[0], expected_event);

            ddog_free_traces(traces_ptr);
        }
    }

    #[test]
    fn test_full_span() {
        unsafe {
            let traces_ptr = ddog_get_traces();
            let trace_ptr = ddog_traces_new_trace(traces_ptr);
            let span_ptr = ddog_trace_new_span(trace_ptr);
            let link_ptr = ddog_span_new_link(span_ptr);

            ddog_set_link_trace_id(link_ptr, 10);
            ddog_set_link_span_id(link_ptr, 20);
            ddog_set_link_flags(link_ptr, 30);

            let event_ptr = ddog_span_new_event(span_ptr);

            ddog_set_event_time(event_ptr, 123456);
            ddog_set_event_name(event_ptr, CharSlice::from("event_name"));

            ddog_set_span_service(span_ptr, CharSlice::from("service"));
            ddog_set_span_name(span_ptr, CharSlice::from("operation"));
            ddog_set_span_resource(span_ptr, CharSlice::from("resource"));
            ddog_set_span_type(span_ptr, CharSlice::from("type"));
            ddog_set_span_trace_id(span_ptr, 1);
            ddog_set_span_span_id(span_ptr, 2);
            ddog_set_span_parent_id(span_ptr, 3);
            ddog_set_span_start(span_ptr, 4);
            ddog_set_span_duration(span_ptr, 5);
            ddog_set_span_error(span_ptr, 6);
            ddog_add_span_meta(
                span_ptr,
                CharSlice::from("meta_key"),
                CharSlice::from("meta_value"),
            );
            ddog_add_span_metrics(span_ptr, CharSlice::from("metric_key"), 1.0);
            ddog_add_span_meta_struct(
                span_ptr,
                CharSlice::from("meta_struct_key"),
                CharSlice::from("meta_struct_value"),
            );

            let span_ref = &*span_ptr;
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

            assert_eq!(*span_ref, expected_span);

            ddog_free_traces(traces_ptr);
        }
    }
}
