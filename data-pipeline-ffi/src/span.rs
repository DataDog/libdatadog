// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::span::{
    AttributeAnyValueBytes, AttributeArrayValueBytes, SpanBytes, SpanEventBytes, SpanLinkBytes,
};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use std::mem;
use tinybytes::{Bytes, BytesString};

macro_rules! span_create_pointer {
    ($fn_name:ident, $struct_type:ty) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name() -> *mut $struct_type {
            Box::into_raw(Box::new(<$struct_type>::default()))
        }
    };
}

macro_rules! span_free_pointer {
    ($fn_name:ident, $struct_type:ty) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(ptr: *mut $struct_type) {
            if !ptr.is_null() {
                drop(Box::from_raw(ptr));
            }
        }
    };
}

macro_rules! span_string_setter {
    ($fn_name:ident, $struct_type:ty, $field:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(ptr: *mut $struct_type, slice: CharSlice) {
            if ptr.is_null() || slice.is_empty() {
                return;
            }

            let object = &mut *ptr;
            object.$field = BytesString::from_slice(slice.as_bytes()).unwrap_or_default();
        }
    };
}

macro_rules! span_integer_setter {
    ($fn_name:ident, $struct_type:ty, $value_type:ty, $field:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(ptr: *mut $struct_type, value: $value_type) {
            if ptr.is_null() {
                return;
            }

            let object = unsafe { &mut *ptr };
            object.$field = value;
        }
    };
}

macro_rules! span_hashmap_setter {
    ($fn_name:ident, $struct_type:ty, $value_type:ty, $value_expr:expr, $field:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut $struct_type,
            key: CharSlice,
            value: $value_type,
        ) {
            if ptr.is_null() {
                return;
            }

            let object = &mut *ptr;
            let bytes_str_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();
            let converted_value = $value_expr(value);
            object.$field.insert(bytes_str_key, converted_value);
        }
    };
}

macro_rules! span_event_attribute_setter {
    ($fn_name:ident, $value_type:ty, $value_expr:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(
            ptr: *mut SpanEventBytes,
            key: CharSlice,
            value: $value_type,
        ) {
            if ptr.is_null() {
                return;
            }

            let span_event = &mut *ptr;
            let attribute_key = BytesString::from_slice(key.as_bytes()).unwrap_or_default();
            let attribute_value = $value_expr(value);

            span_event
                .attributes
                .entry(attribute_key)
                .and_modify(|existing| match existing {
                    AttributeAnyValueBytes::SingleValue(_existing_value) => {
                        let previous =
                            mem::replace(existing, AttributeAnyValueBytes::Array(vec![]));
                        if let AttributeAnyValueBytes::SingleValue(previous_value) = previous {
                            *existing = AttributeAnyValueBytes::Array(vec![
                                previous_value,
                                attribute_value.clone(),
                            ]);
                        }
                    }
                    AttributeAnyValueBytes::Array(ref mut vec_items) => {
                        vec_items.push(attribute_value.clone());
                    }
                })
                .or_insert(AttributeAnyValueBytes::SingleValue(attribute_value));
        }
    };
}

macro_rules! span_pointer_setter {
    ($fn_name:ident, $ptr_type:ty, $field:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $fn_name(span_ptr: *mut SpanBytes, value_ptr: *mut $ptr_type) {
            if span_ptr.is_null() || value_ptr.is_null() {
                return;
            }

            let span = &mut *span_ptr;
            let value = &*value_ptr;
            span.$field.push(value.clone());
        }
    };
}

// Span Setters

span_create_pointer!(ddog_get_span, SpanBytes);
span_free_pointer!(ddog_free_span, SpanBytes);

span_string_setter!(ddog_set_span_service, SpanBytes, service);
span_string_setter!(ddog_set_span_name, SpanBytes, name);
span_string_setter!(ddog_set_span_resource, SpanBytes, resource);
span_string_setter!(ddog_set_span_type, SpanBytes, r#type);

span_integer_setter!(ddog_set_span_trace_id, SpanBytes, u64, trace_id);
span_integer_setter!(ddog_set_span_span_id, SpanBytes, u64, span_id);
span_integer_setter!(ddog_set_span_parent_id, SpanBytes, u64, parent_id);
span_integer_setter!(ddog_set_span_start, SpanBytes, i64, start);
span_integer_setter!(ddog_set_span_duration, SpanBytes, i64, duration);
span_integer_setter!(ddog_set_span_error, SpanBytes, i32, error);

span_hashmap_setter!(
    ddog_add_span_meta,
    SpanBytes,
    CharSlice,
    |value: CharSlice| { BytesString::from_slice(value.as_bytes()).unwrap_or_default() },
    meta
);

span_hashmap_setter!(
    ddog_add_span_metrics,
    SpanBytes,
    f64,
    |value: f64| { value },
    metrics
);

span_hashmap_setter!(
    ddog_add_span_meta_struct,
    SpanBytes,
    CharSlice,
    |value: CharSlice| { Bytes::copy_from_slice(value.as_bytes()) },
    meta_struct
);

span_pointer_setter!(ddog_set_span_link, SpanLinkBytes, span_links);
span_pointer_setter!(ddog_set_span_event, SpanEventBytes, span_events);

// Span Link Setters
span_create_pointer!(ddog_get_link, SpanLinkBytes);
span_free_pointer!(ddog_free_link, SpanLinkBytes);

span_string_setter!(ddog_set_link_tracestate, SpanLinkBytes, tracestate);

span_integer_setter!(ddog_set_link_trace_id, SpanLinkBytes, u64, trace_id);
span_integer_setter!(
    ddog_set_link_trace_id_high,
    SpanLinkBytes,
    u64,
    trace_id_high
);
span_integer_setter!(ddog_set_link_span_id, SpanLinkBytes, u64, span_id);
span_integer_setter!(ddog_set_link_flags, SpanLinkBytes, u64, flags);

span_hashmap_setter!(
    ddog_add_link_attributes,
    SpanLinkBytes,
    CharSlice,
    |value: CharSlice| { BytesString::from_slice(value.as_bytes()).unwrap_or_default() },
    attributes
);

// Span Event Setters
span_create_pointer!(ddog_get_event, SpanEventBytes);
span_free_pointer!(ddog_free_event, SpanEventBytes);

span_string_setter!(ddog_set_event_name, SpanEventBytes, name);

span_integer_setter!(ddog_set_event_time, SpanEventBytes, u64, time_unix_nano);

span_event_attribute_setter!(
    ddog_add_event_attributes_str,
    CharSlice,
    |value: CharSlice| {
        AttributeArrayValueBytes::String(
            BytesString::from_slice(value.as_bytes()).unwrap_or_default(),
        )
    }
);

span_event_attribute_setter!(ddog_add_event_attributes_bool, bool, |value: bool| {
    AttributeArrayValueBytes::Boolean(value)
});

span_event_attribute_setter!(ddog_add_event_attributes_int, i64, |value: i64| {
    AttributeArrayValueBytes::Integer(value)
});

span_event_attribute_setter!(ddog_add_event_attributes_float, f64, |value: f64| {
    AttributeArrayValueBytes::Double(value)
});

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_utils::span::SpanBytes;
    use std::collections::HashMap;
    use std::string::String;

    fn get_bytes_str(value: &'static str) -> BytesString {
        From::from(value)
    }

    fn get_bytes(value: &'static str) -> Bytes {
        From::from(String::from(value))
    }

    #[test]
    fn test_empty_link() {
        unsafe {
            let empty_link_ptr = ddog_get_link();
            let empty_link = &*empty_link_ptr;
            let default_link = SpanLinkBytes::default();

            assert_eq!(*empty_link, default_link);

            ddog_free_link(empty_link_ptr);
        };
    }

    #[test]
    fn test_empty_event() {
        unsafe {
            let empty_event_ptr = ddog_get_event();
            let empty_event = &*empty_event_ptr;
            let default_event = SpanEventBytes::default();

            assert_eq!(*empty_event, default_event);

            ddog_free_event(empty_event_ptr);
        };
    }

    #[test]
    fn test_empty_span() {
        unsafe {
            let empty_span_ptr = ddog_get_span();
            let empty_span = &*empty_span_ptr;
            let default_span = SpanBytes::default();

            assert_eq!(*empty_span, default_span);

            ddog_free_span(empty_span_ptr);
        };
    }

    #[test]
    fn test_full_link() {
        unsafe {
            let link_ptr = ddog_get_link();

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

            let link = &*link_ptr;
            let expected_link = SpanLinkBytes {
                trace_id: 1,
                trace_id_high: 2,
                span_id: 3,
                attributes: HashMap::from([(get_bytes_str("attribute"), get_bytes_str("value"))]),
                tracestate: get_bytes_str("tracestate"),
                flags: 4,
            };

            assert_eq!(*link, expected_link);

            ddog_free_link(link_ptr);
        };
    }

    #[test]
    fn test_full_event() {
        unsafe {
            let event_ptr = ddog_get_event();

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

            let event = &*event_ptr;
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

            assert_eq!(*event, expected_event);

            ddog_free_event(event_ptr);
        };
    }

    #[test]
    fn test_full_span() {
        unsafe {
            let span_ptr = ddog_get_span();
            let link_ptr = ddog_get_link();
            let event_ptr = ddog_get_event();

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

            ddog_set_event_time(event_ptr, 1);
            ddog_set_event_name(event_ptr, CharSlice::from("name"));
            ddog_add_event_attributes_str(
                event_ptr,
                CharSlice::from("str_attribute"),
                CharSlice::from("value"),
            );

            ddog_set_span_service(span_ptr, CharSlice::from("service"));
            ddog_set_span_name(span_ptr, CharSlice::from("span"));
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
                CharSlice::from("meta_attribute"),
                CharSlice::from("meta_value"),
            );
            ddog_add_span_metrics(span_ptr, CharSlice::from("metrics_attribute"), 1.0);
            ddog_add_span_meta_struct(
                span_ptr,
                CharSlice::from("meta_struct_attribute"),
                CharSlice::from("meta_struct_value"),
            );
            ddog_set_span_link(span_ptr, link_ptr);
            ddog_set_span_event(span_ptr, event_ptr);

            let span = &*span_ptr;
            let expected_span = SpanBytes {
                service: get_bytes_str("service"),
                name: get_bytes_str("span"),
                resource: get_bytes_str("resource"),
                r#type: get_bytes_str("type"),
                trace_id: 1,
                span_id: 2,
                parent_id: 3,
                start: 4,
                duration: 5,
                error: 6,
                meta: HashMap::from([(
                    get_bytes_str("meta_attribute"),
                    get_bytes_str("meta_value"),
                )]),
                metrics: HashMap::from([(get_bytes_str("metrics_attribute"), 1.0)]),
                meta_struct: HashMap::from([(
                    get_bytes_str("meta_struct_attribute"),
                    get_bytes("meta_struct_value"),
                )]),
                span_links: vec![SpanLinkBytes {
                    trace_id: 1,
                    trace_id_high: 2,
                    span_id: 3,
                    attributes: HashMap::from([(
                        get_bytes_str("attribute"),
                        get_bytes_str("value"),
                    )]),
                    tracestate: get_bytes_str("tracestate"),
                    flags: 4,
                }],
                span_events: vec![SpanEventBytes {
                    time_unix_nano: 1,
                    name: get_bytes_str("name"),
                    attributes: HashMap::from([(
                        get_bytes_str("str_attribute"),
                        AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::String(
                            get_bytes_str("value"),
                        )),
                    )]),
                }],
            };

            assert_eq!(*span, expected_span);

            ddog_free_span(span_ptr);
            ddog_free_link(link_ptr);
            ddog_free_event(event_ptr);
        };
    }
}