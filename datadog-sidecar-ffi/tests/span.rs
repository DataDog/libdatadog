// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_sidecar_ffi::span::*;
use datadog_trace_utils::span::*;
use ddcommon_ffi::slice::*;
use std::collections::HashMap;
use tinybytes::*;

#[test]
#[cfg_attr(miri, ignore)]
fn test_set_get_all_core_fields() {
    let mut traces = ddog_get_traces();
    let trace = ddog_traces_new_trace(&mut traces);
    let span = ddog_trace_new_span(trace);

    ddog_set_span_service(span, CharSlice::from("my-service"));
    assert_eq!(ddog_get_span_service(span), CharSlice::from("my-service"));

    ddog_set_span_name(span, CharSlice::from("my-span"));
    assert_eq!(ddog_get_span_name(span), CharSlice::from("my-span"));

    ddog_set_span_resource(span, CharSlice::from("my-resource"));
    assert_eq!(ddog_get_span_resource(span), CharSlice::from("my-resource"));

    ddog_set_span_type(span, CharSlice::from("web"));
    assert_eq!(ddog_get_span_type(span), CharSlice::from("web"));

    ddog_set_span_trace_id(span, 123);
    assert_eq!(ddog_get_span_trace_id(span), 123);

    ddog_set_span_id(span, 456);
    assert_eq!(ddog_get_span_id(span), 456);

    ddog_set_span_parent_id(span, 789);
    assert_eq!(ddog_get_span_parent_id(span), 789);

    ddog_set_span_start(span, 1000);
    assert_eq!(ddog_get_span_start(span), 1000);

    ddog_set_span_duration(span, 5000);
    assert_eq!(ddog_get_span_duration(span), 5000);

    ddog_set_span_error(span, 1);
    assert_eq!(ddog_get_span_error(span), 1);

    ddog_free_traces(traces);
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_meta_crud() {
    unsafe {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(&mut traces);
        let span = ddog_trace_new_span(trace);

        let key = CharSlice::from("foo");
        let val = CharSlice::from("bar");

        let key2 = CharSlice::from("foo2");
        let val2 = CharSlice::from("baz");

        assert!(!ddog_has_span_meta(span, key));
        assert!(!ddog_has_span_meta(span, key2));

        ddog_add_span_meta(span, key, val);
        ddog_add_span_meta(span, key, val); // Check for duplicates
        ddog_add_span_meta(span, key2, val2);

        assert!(ddog_has_span_meta(span, key));
        assert!(ddog_has_span_meta(span, key2));

        assert_eq!(ddog_get_span_meta(span, key), val);
        assert_eq!(ddog_get_span_meta(span, key2), val2);

        let mut count = 0;
        let keys = ddog_span_meta_get_keys(span, &mut count);
        let keys_slice = std::slice::from_raw_parts(keys, count);
        assert_eq!(count, 2);
        assert!(keys_slice.iter().any(|k| k == &key));
        assert!(keys_slice.iter().any(|k| k == &key2));

        ddog_del_span_meta(span, key);
        assert!(!ddog_has_span_meta(span, key));

        ddog_span_free_keys_ptr(keys, count);
        ddog_free_traces(traces);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_metrics_crud() {
    unsafe {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(&mut traces);
        let span = ddog_trace_new_span(trace);

        let key = CharSlice::from("load");
        let val = 42.0;

        let key2 = CharSlice::from("load2");
        let val2 = 84.0;

        assert!(!ddog_has_span_metrics(span, key));
        assert!(!ddog_has_span_metrics(span, key2));

        ddog_add_span_metrics(span, key, val);
        ddog_add_span_metrics(span, key, val); // Check for duplicates
        ddog_add_span_metrics(span, key2, val2);

        assert!(ddog_has_span_metrics(span, key));
        assert!(ddog_has_span_metrics(span, key2));

        let mut result = 0.0;
        assert!(ddog_get_span_metrics(span, key, &mut result));
        assert_eq!(result, val);
        assert!(ddog_get_span_metrics(span, key2, &mut result));
        assert_eq!(result, val2);

        let mut count = 0;
        let keys = ddog_span_metrics_get_keys(span, &mut count);
        let keys_slice = std::slice::from_raw_parts(keys, count);
        assert_eq!(count, 2);
        assert!(keys_slice.iter().any(|k| k == &key));
        assert!(keys_slice.iter().any(|k| k == &key2));

        ddog_del_span_metrics(span, key);
        assert!(!ddog_has_span_metrics(span, key));

        ddog_span_free_keys_ptr(keys, count);
        ddog_free_traces(traces);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_meta_struct_crud() {
    unsafe {
        let mut traces = ddog_get_traces();
        let trace = ddog_traces_new_trace(&mut traces);
        let span = ddog_trace_new_span(trace);

        let key = CharSlice::from("bin");
        let val = CharSlice::from("binary_value");

        let key2 = CharSlice::from("bin2");
        let val2 = CharSlice::from("another_binary_value");

        assert!(!ddog_has_span_meta_struct(span, key));
        assert!(!ddog_has_span_meta_struct(span, key2));

        ddog_add_span_meta_struct(span, key, val);
        ddog_add_span_meta_struct(span, key, val); // Check for duplicates
        ddog_add_span_meta_struct(span, key2, val2);

        assert!(ddog_has_span_meta_struct(span, key));
        assert!(ddog_has_span_meta_struct(span, key2));

        assert_eq!(ddog_get_span_meta_struct(span, key), val);
        assert_eq!(ddog_get_span_meta_struct(span, key2), val2);

        let mut count = 0;
        let keys = ddog_span_meta_struct_get_keys(span, &mut count);
        let keys_slice = std::slice::from_raw_parts(keys, count);
        assert_eq!(count, 2);
        assert!(keys_slice.iter().any(|k| k == &key));
        assert!(keys_slice.iter().any(|k| k == &key2));

        ddog_del_span_meta_struct(span, key);
        assert!(!ddog_has_span_meta_struct(span, key));

        ddog_span_free_keys_ptr(keys, count);
        ddog_free_traces(traces);
    }
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_span_debug_log_output() {
    let mut traces = ddog_get_traces();
    let trace = ddog_traces_new_trace(&mut traces);
    let span = ddog_trace_new_span(trace);

    ddog_set_span_name(span, CharSlice::from("debug-span"));
    let debug_output = ddog_span_debug_log(span);

    let expected_output = CharSlice::from("Span { service: , name: debug-span, resource: , type: , trace_id: 0, span_id: 0, parent_id: 0, start: 0, duration: 0, error: 0, meta: {}, metrics: {}, meta_struct: {}, span_links: [], span_events: [] }");

    assert_eq!(debug_output, expected_output);

    ddog_free_charslice(debug_output);
    ddog_free_traces(traces);
}

fn get_bytes_str(value: &'static str) -> BytesString {
    From::from(value)
}
fn get_bytes(value: &'static str) -> Bytes {
    From::from(String::from(value))
}

#[test]
#[cfg_attr(miri, ignore)]
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
#[cfg_attr(miri, ignore)]
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
#[cfg_attr(miri, ignore)]
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
