// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::span::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use tinybytes::{Bytes, BytesString};

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

// ------------------- SpanBytes -------------------

#[no_mangle]
pub extern "C" fn ddog_get_span() -> *mut SpanBytes {
    Box::into_raw(Box::default())
}

#[no_mangle]
pub unsafe extern "C" fn ddog_free_span(ptr: *mut SpanBytes) {
    if !ptr.is_null() {
        drop(Box::from_raw(ptr));
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_service(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, service);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_name(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, name);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_resource(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, resource);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_type(ptr: *mut SpanBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, r#type);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_trace_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, trace_id);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_span_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, span_id);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_parent_id(ptr: *mut SpanBytes, value: u64) {
    set_numeric_field!(ptr, value, parent_id);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_start(ptr: *mut SpanBytes, value: i64) {
    set_numeric_field!(ptr, value, start);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_duration(ptr: *mut SpanBytes, value: i64) {
    set_numeric_field!(ptr, value, duration);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_span_error(ptr: *mut SpanBytes, value: i32) {
    set_numeric_field!(ptr, value, error);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_add_span_meta(ptr: *mut SpanBytes, key: CharSlice, val: CharSlice) {
    insert_hashmap!(
        ptr,
        key,
        BytesString::from_slice(val.as_bytes()).unwrap_or_default(),
        meta
    );
}

#[no_mangle]
pub unsafe extern "C" fn ddog_add_span_metrics(ptr: *mut SpanBytes, key: CharSlice, val: f64) {
    insert_hashmap!(ptr, key, val, metrics);
}

#[no_mangle]
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

// ------------------- SpanLinkBytes -------------------

#[no_mangle]
pub unsafe extern "C" fn ddog_span_new_link(span_ptr: *mut SpanBytes) -> *mut SpanLinkBytes {
    new_vector_item!(span_ptr, span_links, SpanLinkBytes)
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_link_tracestate(ptr: *mut SpanLinkBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, tracestate);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_link_trace_id(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, trace_id);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_link_trace_id_high(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, trace_id_high);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_link_span_id(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, span_id);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_link_flags(ptr: *mut SpanLinkBytes, val: u64) {
    set_numeric_field!(ptr, val, flags);
}

#[no_mangle]
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
pub unsafe extern "C" fn ddog_span_new_event(span_ptr: *mut SpanBytes) -> *mut SpanEventBytes {
    new_vector_item!(span_ptr, span_events, SpanEventBytes)
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_event_name(ptr: *mut SpanEventBytes, slice: CharSlice) {
    set_string_field!(ptr, slice, name);
}

#[no_mangle]
pub unsafe extern "C" fn ddog_set_event_time(ptr: *mut SpanEventBytes, val: u64) {
    set_numeric_field!(ptr, val, time_unix_nano);
}

#[no_mangle]
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
pub unsafe extern "C" fn ddog_add_event_attributes_bool(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: bool,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Boolean(val));
}

#[no_mangle]
pub unsafe extern "C" fn ddog_add_event_attributes_int(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: i64,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Integer(val));
}

#[no_mangle]
pub unsafe extern "C" fn ddog_add_event_attributes_float(
    ptr: *mut SpanEventBytes,
    key: CharSlice,
    val: f64,
) {
    set_event_attribute!(ptr, key, AttributeArrayValueBytes::Double(val));
}

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
            let empty_span_ptr = ddog_get_span();
            let empty_span = &*empty_span_ptr;
            let default_span = SpanBytes::default();

            assert_eq!(*empty_span, default_span);

            ddog_free_span(empty_span_ptr);
        }
    }

    #[test]
    fn test_empty_event() {
        unsafe {
            let span_ptr = ddog_get_span();

            let event_ptr = ddog_span_new_event(span_ptr);
            let event_ref = &*event_ptr;

            let default_event = SpanEventBytes::default();
            assert_eq!(*event_ref, default_event);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_events.len(), 1);

            ddog_free_span(span_ptr);
        }
    }

    #[test]
    fn test_empty_link() {
        unsafe {
            let span_ptr = ddog_get_span();

            let link_ptr = ddog_span_new_link(span_ptr);

            let link_ref = &*link_ptr;
            let expected_link = SpanLinkBytes::default();
            assert_eq!(*link_ref, expected_link);

            let span_ref = &*span_ptr;
            assert_eq!(span_ref.span_links.len(), 1);

            ddog_free_span(span_ptr);
        }
    }

    #[test]
    fn test_full_link() {
        unsafe {
            let span_ptr = ddog_get_span();

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

            ddog_free_span(span_ptr);
        }
    }

    #[test]
    fn test_full_event() {
        unsafe {
            let span_ptr = ddog_get_span();

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

            ddog_free_span(span_ptr);
        }
    }

    #[test]
    fn test_full_span() {
        unsafe {
            let span_ptr = ddog_get_span();

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

            ddog_free_span(span_ptr);
        }
    }
}
