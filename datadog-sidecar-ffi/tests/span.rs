// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the native V1 payload builder FFI ([`datadog_sidecar_ffi::span`]). They
//! fill and read the payload through the exported `extern "C"` functions only, as a tracer does.

use datadog_sidecar_ffi::span::*;
use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
use libdd_trace_utils::span::v1::{SpanBytes, TraceChunkBytes, TracerPayloadBytes};
use std::collections::HashMap;
use std::ffi::CStr;

fn cs(s: &str) -> CharSlice<'_> {
    CharSlice::from(s)
}

fn string(slice: CharSlice) -> String {
    slice.to_utf8_lossy().into_owned()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The entries of an attribute map by key; map order is unspecified, so tests never index it.
unsafe fn attrs_by_key(attrs: *const Attributes) -> HashMap<String, *const AttrValue> {
    unsafe {
        (0..ddog_v1_attributes_len(&*attrs))
            .map(|i| {
                (
                    string(ddog_v1_attributes_key(&*attrs, i)),
                    ddog_v1_attributes_value(&*attrs, i),
                )
            })
            .collect()
    }
}

/// A builder with one chunk holding one span, the common fixture.
fn one_span(
    trace_id_low: u64,
) -> (
    Box<TracerPayloadBytes>,
    *mut TraceChunkBytes,
    *mut SpanBytes,
) {
    let mut b = ddog_v1_new_builder();
    let chunk = ddog_new_chunk(&mut b, 0, trace_id_low);
    let span = unsafe { ddog_new_span(&mut *chunk) };
    (b, chunk, span)
}

#[test]
fn builds_span_with_promoted_and_typed_attributes() {
    let (b, _, span) = one_span(0x0123456789abcdef);
    unsafe {
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_set_span_name(&mut *span, cs("op"));
        ddog_set_span_resource(&mut *span, cs("res"));
        ddog_span_set_id(&mut *span, 42);
        ddog_span_set_start(&mut *span, 1_000);
        ddog_span_set_duration(&mut *span, 500);
        ddog_span_set_error(&mut *span, true);
        ddog_set_span_kind(&mut *span, 2); // Server
        let attrs = ddog_span_get_attributes(span);
        ddog_attributes_add_str(&mut *attrs, cs("k_str"), cs("v_str"));
        ddog_attributes_add_int(&mut *attrs, cs("k_int"), 7);
    }

    let encoded = to_vec_from_v1(&b);
    let expected_tid = [
        0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,
    ];
    assert!(contains(&encoded, &expected_tid));
    for s in [b"svc" as &[u8], b"op", b"res", b"k_str", b"v_str", b"k_int"] {
        assert!(
            contains(&encoded, s),
            "{} should appear",
            String::from_utf8_lossy(s)
        );
    }
    // SpanKind Server = 2: key 16 (0x10) then uint 2 (0x02)
    assert!(contains(&encoded, &[0x10, 0x02]));
}

#[test]
fn getters_round_trip_setters() {
    let mut b = ddog_v1_new_builder();
    let chunk = ddog_new_chunk(&mut b, 0xaabb, 0xccdd);
    unsafe {
        ddog_set_chunk_sampling_priority(&mut *chunk, 2);
        ddog_set_chunk_origin(&mut *chunk, cs("lambda"));
        ddog_set_chunk_sampling_mechanism(&mut *chunk, 4);
        ddog_attributes_add_str(
            &mut *ddog_chunk_get_attributes(chunk),
            cs("c_key"),
            cs("c_val"),
        );
        ddog_attributes_add_str(
            &mut *ddog_payload_get_attributes(&mut *b),
            cs("p_key"),
            cs("p_val"),
        );
    }

    let span = unsafe { ddog_new_span(&mut *chunk) };
    let (link, event);
    unsafe {
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_set_span_name(&mut *span, cs("op"));
        ddog_set_span_resource(&mut *span, cs("res"));
        ddog_set_span_type(&mut *span, cs("web"));
        ddog_set_span_env(&mut *span, cs("prod"));
        ddog_set_span_version(&mut *span, cs("1.2.3"));
        ddog_set_span_component(&mut *span, cs("pdo"));
        ddog_span_set_id(&mut *span, 42);
        ddog_span_set_parent_id(&mut *span, 7);
        ddog_span_set_start(&mut *span, 1_000);
        ddog_span_set_duration(&mut *span, 500);
        ddog_span_set_error(&mut *span, true);
        ddog_set_span_kind(&mut *span, 3); // Client
        let attrs = ddog_span_get_attributes(span);
        ddog_attributes_add_str(&mut *attrs, cs("a_str"), cs("v"));
        ddog_attributes_add_int(&mut *attrs, cs("a_int"), 11);
        ddog_attributes_add_double(&mut *attrs, cs("a_dbl"), 1.5);
        ddog_attributes_add_bool(&mut *attrs, cs("a_bool"), true);
        ddog_attributes_add_bytes(&mut *attrs, cs("a_bytes"), cs("raw"));

        link = ddog_new_link(&mut *span);
        ddog_link_set_trace_id(&mut *link, 0x11, 0x22);
        ddog_link_set_span_id(&mut *link, 9);
        ddog_link_set_tracestate(&mut *link, cs("dd=s:1"));
        ddog_attributes_add_str(
            &mut *ddog_link_get_attributes(link),
            cs("l_key"),
            cs("l_val"),
        );

        event = ddog_new_event(&mut *span);
        ddog_event_set_time(&mut *event, 123);
        ddog_event_set_name(&mut *event, cs("exception"));
        ddog_attributes_add_int(&mut *ddog_event_get_attributes(event), cs("e_int"), 5);
    }

    // Tree walk: the index getters hand back the pointers the creators returned.
    assert_eq!(ddog_v1_get_chunk_count(&b), 1);
    assert_eq!(ddog_v1_get_chunk(&mut b, 0), chunk);
    assert!(ddog_v1_get_chunk(&mut b, 1).is_null());
    let (link, event) = unsafe {
        assert_eq!(ddog_chunk_span_count(&*chunk), 1);
        assert_eq!(ddog_v1_get_span(&mut *chunk, 0), span);
        assert!(ddog_v1_get_span(&mut *chunk, 1).is_null());
        assert_eq!(ddog_v1_get_link_count(&*span), 1);
        assert_eq!(ddog_v1_get_event_count(&*span), 1);
        assert_eq!(ddog_v1_get_link(&mut *span, 0), link);
        assert_eq!(ddog_v1_get_event(&mut *span, 0), event);
        assert!(ddog_v1_get_link(&mut *span, 1).is_null());
        // Read back through the re-fetched pointers, as a reader walking the tree does.
        (
            ddog_v1_get_link(&mut *span, 0),
            ddog_v1_get_event(&mut *span, 0),
        )
    };
    let (s, l, e) = unsafe { (&*span, &*link, &*event) };

    // Chunk and payload.
    let c = unsafe { &*chunk };
    assert_eq!(ddog_v1_get_chunk_trace_id_high(c), 0xaabb);
    assert_eq!(ddog_v1_get_chunk_trace_id_low(c), 0xccdd);
    let mut prio = 0;
    assert!(ddog_v1_get_chunk_sampling_priority(c, &mut prio));
    assert_eq!(prio, 2);
    let mut mech = 0;
    assert!(ddog_v1_get_chunk_sampling_mechanism(c, &mut mech));
    assert_eq!(mech, 4);
    assert_eq!(string(ddog_v1_get_chunk_origin(c)), "lambda");
    unsafe {
        let chunk_attrs = attrs_by_key(ddog_chunk_get_attributes(chunk));
        assert_eq!(string(ddog_v1_value_str(&*chunk_attrs["c_key"])), "c_val");
        let payload_attrs = attrs_by_key(ddog_payload_get_attributes(&mut *b));
        assert_eq!(string(ddog_v1_value_str(&*payload_attrs["p_key"])), "p_val");
        assert!(!payload_attrs.contains_key("missing"));
    }

    // Span.
    assert_eq!(string(ddog_v1_get_span_service(s)), "svc");
    assert_eq!(string(ddog_v1_get_span_name(s)), "op");
    assert_eq!(string(ddog_v1_get_span_resource(s)), "res");
    assert_eq!(string(ddog_v1_get_span_type(s)), "web");
    assert_eq!(string(ddog_v1_get_span_env(s)), "prod");
    assert_eq!(string(ddog_v1_get_span_version(s)), "1.2.3");
    assert_eq!(string(ddog_v1_get_span_component(s)), "pdo");
    assert_eq!(ddog_v1_get_span_id(s), 42);
    assert_eq!(ddog_v1_get_span_parent_id(s), 7);
    assert_eq!(ddog_v1_get_span_start(s), 1_000);
    assert_eq!(ddog_v1_get_span_duration(s), 500);
    assert!(ddog_v1_get_span_error(s));
    assert_eq!(ddog_v1_get_span_kind(s), 3);
    unsafe {
        let attrs = attrs_by_key(ddog_span_get_attributes(span));
        assert_eq!(attrs.len(), 5);
        assert_eq!(ddog_v1_value_type(&*attrs["a_str"]), DDOG_V1_ATTR_STRING);
        assert_eq!(string(ddog_v1_value_str(&*attrs["a_str"])), "v");
        assert_eq!(ddog_v1_value_type(&*attrs["a_int"]), DDOG_V1_ATTR_INT);
        assert_eq!(ddog_v1_value_int(&*attrs["a_int"]), 11);
        assert_eq!(ddog_v1_value_type(&*attrs["a_dbl"]), DDOG_V1_ATTR_DOUBLE);
        assert_eq!(ddog_v1_value_double(&*attrs["a_dbl"]), 1.5);
        assert_eq!(ddog_v1_value_type(&*attrs["a_bool"]), DDOG_V1_ATTR_BOOL);
        assert!(ddog_v1_value_bool(&*attrs["a_bool"]));
        assert_eq!(ddog_v1_value_type(&*attrs["a_bytes"]), DDOG_V1_ATTR_BYTES);
        assert_eq!(string(ddog_v1_value_bytes(&*attrs["a_bytes"])), "raw");
        // A typed getter on another type returns the default.
        assert_eq!(ddog_v1_value_int(&*attrs["a_str"]), 0);
        assert!(ddog_v1_attributes_value(&*ddog_span_get_attributes(span), 99).is_null());
    }

    // Link and event.
    assert_eq!(ddog_v1_get_link_trace_id_high(l), 0x11);
    assert_eq!(ddog_v1_get_link_trace_id_low(l), 0x22);
    assert_eq!(ddog_v1_get_link_span_id(l), 9);
    assert_eq!(string(ddog_v1_get_link_tracestate(l)), "dd=s:1");
    assert_eq!(ddog_v1_get_event_time(e), 123);
    assert_eq!(string(ddog_v1_get_event_name(e)), "exception");
    unsafe {
        let lattrs = attrs_by_key(ddog_link_get_attributes(link));
        assert_eq!(string(ddog_v1_value_str(&*lattrs["l_key"])), "l_val");
        let eattrs = attrs_by_key(ddog_event_get_attributes(event));
        assert_eq!(ddog_v1_value_int(&*eattrs["e_int"]), 5);
    }

    ddog_v1_free_builder(b);
}

#[test]
fn nested_attributes_round_trip() {
    let (b, _, span) = one_span(1);
    unsafe {
        // root: { a: "x", items: ["first", 42, { flag: true }] }
        let mut inner = ddog_attr_map_new(1);
        ddog_attributes_add_bool(
            &mut *ddog_attr_map_get_attributes(&mut inner),
            cs("flag"),
            true,
        );
        let mut items = ddog_attr_list_new(3);
        ddog_attr_list_push_str(&mut items, cs("first"));
        ddog_attr_list_push_int(&mut items, 42);
        ddog_attr_list_push_map(&mut items, inner);
        let mut root = ddog_attr_map_new(2);
        ddog_attributes_add_str(
            &mut *ddog_attr_map_get_attributes(&mut root),
            cs("a"),
            cs("x"),
        );
        ddog_attributes_add_list(
            &mut *ddog_attr_map_get_attributes(&mut root),
            cs("items"),
            items,
        );
        ddog_attributes_add_map(&mut *ddog_span_get_attributes(span), cs("root"), root);
        let mut nums = ddog_attr_list_new(2);
        ddog_attr_list_push_double(&mut nums, 1.5);
        let mut nested = ddog_attr_list_new(1);
        ddog_attr_list_push_bool(&mut nested, false);
        ddog_attr_list_push_list(&mut nums, nested);
        ddog_attributes_add_list(&mut *ddog_span_get_attributes(span), cs("nums"), nums);

        let attrs = attrs_by_key(ddog_span_get_attributes(span));
        assert_eq!(ddog_v1_value_type(&*attrs["root"]), DDOG_V1_ATTR_KEYVALUE);
        let root = attrs_by_key(ddog_v1_value_map(&*attrs["root"]));
        assert_eq!(string(ddog_v1_value_str(&*root["a"])), "x");
        let items = root["items"];
        assert_eq!(ddog_v1_value_type(&*items), DDOG_V1_ATTR_LIST);
        assert_eq!(ddog_v1_value_list_len(&*items), 3);
        assert_eq!(
            string(ddog_v1_value_str(&*ddog_v1_value_list_get(&*items, 0))),
            "first"
        );
        assert_eq!(ddog_v1_value_int(&*ddog_v1_value_list_get(&*items, 1)), 42);
        let inner = attrs_by_key(ddog_v1_value_map(&*ddog_v1_value_list_get(&*items, 2)));
        assert!(ddog_v1_value_bool(&*inner["flag"]));
        assert!(ddog_v1_value_list_get(&*items, 3).is_null());
        assert!(ddog_v1_value_map(&*items).is_null());

        let nums = attrs["nums"];
        assert_eq!(
            ddog_v1_value_double(&*ddog_v1_value_list_get(&*nums, 0)),
            1.5
        );
        let nested = ddog_v1_value_list_get(&*nums, 1);
        assert_eq!(ddog_v1_value_list_len(&*nested), 1);
        assert!(!ddog_v1_value_bool(&*ddog_v1_value_list_get(&*nested, 0)));
    }

    let encoded = to_vec_from_v1(&b);
    for s in [b"root" as &[u8], b"items", b"first", b"flag", b"nums"] {
        assert!(
            contains(&encoded, s),
            "{} should appear",
            String::from_utf8_lossy(s)
        );
    }
}

#[test]
fn an_overwritten_key_is_encoded_once_with_the_last_value() {
    let (b, _, span) = one_span(1);
    unsafe {
        let attrs = ddog_span_get_attributes(span);
        ddog_attributes_add_str(&mut *attrs, cs("dup"), cs("first-value"));
        ddog_attributes_add_str(&mut *attrs, cs("dup"), cs("last-value"));
        // Before dedup the reader sees both entries; the last one wins.
        assert_eq!(ddog_v1_attributes_len(&*attrs), 2);
        assert_eq!(
            string(ddog_v1_value_str(&*attrs_by_key(attrs)["dup"])),
            "last-value"
        );
    }
    let encoded = to_vec_from_v1(&b);
    assert!(contains(&encoded, b"last-value"));
    assert!(!contains(&encoded, b"first-value"));
}

#[test]
fn span_refetched_by_index_after_sibling_pushes() {
    let mut b = ddog_v1_new_builder();
    let first_chunk = ddog_new_chunk(&mut b, 0, 1);
    unsafe {
        let first_span = ddog_new_span(&mut *first_chunk);
        ddog_span_set_id(&mut *first_span, 77);
        ddog_link_set_span_id(&mut *ddog_new_link(&mut *first_span), 88);
    }
    // Enough siblings to reallocate every parent vector; held pointers are re-fetched by index.
    for i in 0..64 {
        let chunk = ddog_new_chunk(&mut b, 0, i + 2);
        unsafe {
            ddog_new_span(&mut *chunk);
            let first_chunk = ddog_v1_get_chunk(&mut b, 0);
            ddog_new_span(&mut *first_chunk);
            ddog_new_link(&mut *ddog_v1_get_span(&mut *first_chunk, 0));
        }
    }
    assert_eq!(ddog_v1_get_chunk_count(&b), 65);
    let first_chunk = ddog_v1_get_chunk(&mut b, 0);
    unsafe {
        assert_eq!(ddog_chunk_span_count(&*first_chunk), 65);
        let first_span = ddog_v1_get_span(&mut *first_chunk, 0);
        assert_eq!(ddog_v1_get_span_id(&*first_span), 77);
        assert_eq!(ddog_v1_get_link_count(&*first_span), 65);
        assert_eq!(
            ddog_v1_get_link_span_id(&*ddog_v1_get_link(&mut *first_span, 0)),
            88
        );
    }
}

#[test]
fn encoder_streams_repeated_string_once() {
    // "shared" used as service in two chunks must appear as raw bytes exactly once on the
    // wire: the encoder's streaming string table emits the second occurrence as a uint id.
    let mut b = ddog_v1_new_builder();
    for (i, name) in [(1, "op1"), (2, "op2")] {
        let chunk = ddog_new_chunk(&mut b, 0, i);
        unsafe {
            let span = ddog_new_span(&mut *chunk);
            ddog_set_span_service(&mut *span, cs("shared"));
            ddog_set_span_name(&mut *span, cs(name));
            ddog_span_set_id(&mut *span, i);
        }
    }
    let encoded = to_vec_from_v1(&b);
    let occurrences = encoded
        .windows(b"shared".len())
        .filter(|w| *w == b"shared")
        .count();
    assert_eq!(
        occurrences, 1,
        "repeated string must be interned on the wire"
    );
}

#[test]
fn builds_links_and_events() {
    let (b, _, span) = one_span(1);
    unsafe {
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_span_set_id(&mut *span, 1);
        let link = ddog_new_link(&mut *span);
        ddog_link_set_trace_id(&mut *link, 0xaa, 0xbb);
        ddog_link_set_span_id(&mut *link, 9);
        ddog_link_set_tracestate(&mut *link, cs("dd=s:1"));
        ddog_attributes_add_str(
            &mut *ddog_link_get_attributes(link),
            cs("link.attr"),
            cs("link.val"),
        );
        let event = ddog_new_event(&mut *span);
        ddog_event_set_time(&mut *event, 123);
        ddog_event_set_name(&mut *event, cs("exception"));
        ddog_attributes_add_int(&mut *ddog_event_get_attributes(event), cs("ev.attr"), 5);
    }

    let encoded = to_vec_from_v1(&b);
    for s in [
        b"exception" as &[u8],
        b"dd=s:1",
        b"link.attr",
        b"link.val",
        b"ev.attr",
    ] {
        assert!(
            contains(&encoded, s),
            "{} should appear",
            String::from_utf8_lossy(s)
        );
    }
    let expected_link_tid = [0, 0, 0, 0, 0, 0, 0, 0xaa, 0, 0, 0, 0, 0, 0, 0, 0xbb];
    assert!(contains(&encoded, &expected_link_tid));
}

#[test]
fn chunk_level_fields_encoded() {
    let (b, chunk, span) = one_span(1);
    unsafe {
        ddog_set_chunk_sampling_priority(&mut *chunk, 2);
        ddog_set_chunk_origin(&mut *chunk, cs("lambda"));
        ddog_set_chunk_sampling_mechanism(&mut *chunk, 4);
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_span_set_id(&mut *span, 1);
    }
    let encoded = to_vec_from_v1(&b);
    assert!(contains(&encoded, b"lambda"));
    // sampling_mechanism = 4 (chunk key 0x07 + fixint 0x04)
    assert!(contains(&encoded, &[0x07, 0x04]));
}

#[test]
fn populate_metadata_sets_container_id_and_fields() {
    let (mut b, _, span) = one_span(1);
    unsafe {
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_span_set_id(&mut *span, 1);
    }
    ddog_set_payload_metadata(&mut b, cs("prod"), cs("4.5.6"), cs("my-host"));
    assert_eq!(string(ddog_v1_get_payload_env(&b)), "prod");
    assert_eq!(string(ddog_v1_get_payload_app_version(&b)), "4.5.6");
    assert_eq!(string(ddog_v1_get_payload_hostname(&b)), "my-host");
    populate_payload_metadata(
        &mut b,
        "container-xyz",
        "php",
        "8.3",
        "1.2.3",
        "runtime-uuid",
    );
    let encoded = to_vec_from_v1(&b);
    for s in [
        b"container-xyz" as &[u8],
        b"php",
        b"8.3",
        b"1.2.3",
        b"runtime-uuid",
        b"prod",
        b"my-host",
        b"4.5.6",
    ] {
        assert!(
            contains(&encoded, s),
            "{} should appear",
            String::from_utf8_lossy(s)
        );
    }
}

#[test]
fn transfer_span_attr_copies_and_optionally_deletes() {
    let (b, chunk, root) = one_span(1);
    let key = |k: &'static CStr| k.as_ptr();
    unsafe {
        let root_attrs = ddog_span_get_attributes(root);
        ddog_attributes_add_str(&mut *root_attrs, cs("error.message"), cs("boom"));
        ddog_attributes_add_double(&mut *root_attrs, cs("_dd.agent_psr"), 0.5);

        // The inferred span is a sibling push, so the root (and its handle) is re-fetched after it.
        let inferred = ddog_new_span(&mut *chunk);
        let root = ddog_v1_get_span(&mut *chunk, 0);
        let root_attrs = ddog_span_get_attributes(root);
        let inferred_attrs = ddog_span_get_attributes(inferred);

        // Copy, keeping the source.
        assert!(ddog_transfer_span_attr(
            &mut *root,
            &mut *inferred,
            key(c"error.message"),
            false
        ));
        assert!(attrs_by_key(root_attrs).contains_key("error.message"));
        let copied = attrs_by_key(inferred_attrs)["error.message"];
        assert_eq!(ddog_v1_value_type(&*copied), DDOG_V1_ATTR_STRING);
        assert_eq!(string(ddog_v1_value_str(&*copied)), "boom");

        // Move, deleting the source; the type is preserved.
        assert!(ddog_transfer_span_attr(
            &mut *root,
            &mut *inferred,
            key(c"_dd.agent_psr"),
            true
        ));
        assert!(!attrs_by_key(root_attrs).contains_key("_dd.agent_psr"));
        let moved = attrs_by_key(inferred_attrs)["_dd.agent_psr"];
        assert_eq!(ddog_v1_value_type(&*moved), DDOG_V1_ATTR_DOUBLE);
        assert_eq!(ddog_v1_value_double(&*moved), 0.5);

        // An absent key is a no-op.
        assert!(!ddog_transfer_span_attr(
            &mut *root,
            &mut *inferred,
            key(c"missing"),
            true
        ));
        assert_eq!(ddog_v1_attributes_len(&*inferred_attrs), 2);
        assert_eq!(ddog_v1_attributes_len(&*root_attrs), 1);
    }
    let encoded = to_vec_from_v1(&b);
    for s in [b"error.message" as &[u8], b"boom", b"_dd.agent_psr"] {
        assert!(
            contains(&encoded, s),
            "{} should appear",
            String::from_utf8_lossy(s)
        );
    }
}

#[test]
fn attributes_handle_stays_valid_across_element_calls() {
    // As the tracer does: take the handles once, then interleave setters and sibling pushes.
    let (b, chunk, span) = one_span(1);
    unsafe {
        let attrs = ddog_span_get_attributes(span);
        ddog_span_set_id(&mut *span, 1);
        ddog_attributes_add_int(&mut *attrs, cs("a"), 1);
        let event = ddog_new_event(&mut *span);
        let event_attrs = ddog_event_get_attributes(event);
        ddog_event_set_name(&mut *event, cs("ev"));
        ddog_attributes_add_int(&mut *event_attrs, cs("e"), 2);
        ddog_event_set_time(&mut *event, 3);
        ddog_attributes_add_int(&mut *event_attrs, cs("f"), 4);
        ddog_set_span_service(&mut *span, cs("svc"));
        ddog_attributes_add_int(&mut *attrs, cs("b"), 5);
        ddog_new_span(&mut *chunk);
        ddog_attributes_add_int(&mut *attrs, cs("c"), 6);
        assert_eq!(ddog_v1_attributes_len(&*attrs), 3);
        assert_eq!(ddog_v1_attributes_len(&*event_attrs), 2);
        assert_eq!(ddog_v1_get_span_id(&*span), 1);
    }
    drop(b);
}

#[test]
fn freeing_a_populated_builder_releases_everything() {
    // Miri's leak checker is the assertion: nothing built here is sent.
    let mut b = ddog_v1_new_builder();
    let chunk = ddog_new_chunk(&mut b, 0, 2);
    unsafe {
        let span = ddog_new_span(&mut *chunk);
        ddog_new_link(&mut *span);
        ddog_new_event(&mut *span);
        let mut m = ddog_attr_map_new(1);
        ddog_attributes_add_int(&mut *ddog_attr_map_get_attributes(&mut m), cs("y"), 1);
        ddog_attributes_add_map(&mut *ddog_span_get_attributes(span), cs("x"), m);
    }
    ddog_v1_free_builder(b);
}

#[test]
fn deep_mixed_nest_builds_at_any_capacity() {
    // [ { inner: [1, b"raw", []], empty: {} }, "tail" ], built with capacity 0 (forcing regrowth)
    // and with exact capacities.
    unsafe fn build(span: *mut SpanBytes, key: &str, cap: impl Fn(usize) -> usize) {
        unsafe {
            let mut inner = ddog_attr_list_new(cap(3));
            ddog_attr_list_push_int(&mut inner, 1);
            ddog_attr_list_push_bytes(&mut inner, cs("raw"));
            ddog_attr_list_push_list(&mut inner, ddog_attr_list_new(cap(0)));
            let mut mid = ddog_attr_map_new(cap(2));
            ddog_attributes_add_list(
                &mut *ddog_attr_map_get_attributes(&mut mid),
                cs("inner"),
                inner,
            );
            ddog_attributes_add_map(
                &mut *ddog_attr_map_get_attributes(&mut mid),
                cs("empty"),
                ddog_attr_map_new(cap(0)),
            );
            let mut outer = ddog_attr_list_new(cap(2));
            ddog_attr_list_push_map(&mut outer, mid);
            ddog_attr_list_push_str(&mut outer, cs("tail"));
            ddog_attributes_add_list(&mut *ddog_span_get_attributes(span), cs(key), outer);
        }
    }

    let (_b, _, span) = one_span(1);
    unsafe {
        build(span, "zero", |_| 0);
        build(span, "exact", |n| n);
        let attrs = attrs_by_key(ddog_span_get_attributes(span));
        for key in ["zero", "exact"] {
            let outer = &*attrs[key];
            assert_eq!(ddog_v1_value_list_len(outer), 2);
            assert_eq!(
                string(ddog_v1_value_str(&*ddog_v1_value_list_get(outer, 1))),
                "tail"
            );
            let mid = attrs_by_key(ddog_v1_value_map(&*ddog_v1_value_list_get(outer, 0)));
            assert_eq!(mid.len(), 2);
            let empty = &*mid["empty"];
            assert_eq!(ddog_v1_value_type(empty), DDOG_V1_ATTR_KEYVALUE);
            assert_eq!(ddog_v1_attributes_len(&*ddog_v1_value_map(empty)), 0);
            let inner = &*mid["inner"];
            assert_eq!(ddog_v1_value_list_len(inner), 3);
            assert_eq!(ddog_v1_value_int(&*ddog_v1_value_list_get(inner, 0)), 1);
            let raw = &*ddog_v1_value_list_get(inner, 1);
            assert_eq!(ddog_v1_value_type(raw), DDOG_V1_ATTR_BYTES);
            assert_eq!(ddog_v1_value_bytes(raw).as_bytes(), b"raw");
            let nested = &*ddog_v1_value_list_get(inner, 2);
            assert_eq!(ddog_v1_value_type(nested), DDOG_V1_ATTR_LIST);
            assert_eq!(ddog_v1_value_list_len(nested), 0);
        }
    }
}

#[test]
fn literal_key_adders_store_typed_values() {
    let (_b, _, span) = one_span(1);
    unsafe {
        let attrs = ddog_span_get_attributes(span);
        ddog_attributes_add_lit(&mut *attrs, c"lit.str".as_ptr(), cs("v"));
        ddog_attributes_add_double_lit(&mut *attrs, c"lit.dbl".as_ptr(), 2.5);
        let by_key = attrs_by_key(attrs);
        assert_eq!(by_key.len(), 2);
        assert_eq!(ddog_v1_value_type(&*by_key["lit.str"]), DDOG_V1_ATTR_STRING);
        assert_eq!(string(ddog_v1_value_str(&*by_key["lit.str"])), "v");
        assert_eq!(ddog_v1_value_type(&*by_key["lit.dbl"]), DDOG_V1_ATTR_DOUBLE);
        assert_eq!(ddog_v1_value_double(&*by_key["lit.dbl"]), 2.5);
    }
}

#[test]
fn span_debug_log_renders_readable_string() {
    let (_b, chunk, span) = one_span(0xdead);
    unsafe {
        ddog_set_span_service(&mut *span, cs("my-service"));
        ddog_set_span_name(&mut *span, cs("my-operation"));
        ddog_set_span_resource(&mut *span, cs("GET /x"));
        ddog_span_set_id(&mut *span, 42);
        ddog_span_set_parent_id(&mut *span, 7);
        ddog_span_set_start(&mut *span, 1_000);
        ddog_span_set_duration(&mut *span, 500);
        ddog_span_set_error(&mut *span, true);
        ddog_set_span_kind(&mut *span, 2); // Server
        ddog_set_span_component(&mut *span, cs("pdo"));
        ddog_attributes_add_int(
            &mut *ddog_span_get_attributes(span),
            cs("http.status_code"),
            200,
        );
        ddog_new_link(&mut *span);
        ddog_new_event(&mut *span);
    }

    let slice = unsafe { ddog_v1_span_debug_log(&*chunk, &*span) };
    let rendered = string(slice);
    for part in [
        "my-service",
        "my-operation",
        "resource=\"GET /x\"",
        "span_id=42",
        "parent_id=7",
        "error=true",
        "kind=Server",
        "component=\"pdo\"",
        "http.status_code=200",
        "links=1",
        "events=1",
    ] {
        assert!(rendered.contains(part), "{part} should appear: {rendered}");
    }
    unsafe { ddog_free_charslice(slice) };
}
