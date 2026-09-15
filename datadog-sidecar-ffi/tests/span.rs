// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the native V1 payload builder ([`datadog_sidecar_ffi::span`]). They drive
//! the builder through its public API and the exported `ddog_v1_*` getters, mirroring how the
//! PHP-side FFI adapter fills the model.

use datadog_sidecar_ffi::span::*;
use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
use libdd_trace_utils::span::v1::{AttributeValueBytes, SpanBytes, SpanKind};
use libdd_trace_utils::span::vec_map::VecMap;
use std::borrow::Cow;

fn cs(s: &str) -> CharSlice<'_> {
    CharSlice::from(s)
}

// Test-only builder helpers reproducing the mutator C-ABI dropped in the bytes.rs V1 pivot (the
// PHP-side FFI now fills the model via `TracerPayloadV1Builder` methods), so the tests below
// still exercise a fully populated payload.

fn to_bytes_string(slice: CharSlice) -> BytesString {
    match String::from_utf8_lossy(slice.as_bytes()) {
        Cow::Owned(s) => s.into(),
        Cow::Borrowed(_) => unsafe {
            BytesString::from_bytes_unchecked(Bytes::from_underlying(slice.as_bytes().to_vec()))
        },
    }
}

fn set_string_field(field: &mut BytesString, slice: CharSlice) {
    if slice.is_empty() {
        return;
    }
    *field = to_bytes_string(slice);
}

fn insert_attr(
    map: &mut VecMap<BytesString, AttributeValueBytes>,
    key: CharSlice,
    value: AttributeValueBytes,
) {
    if key.is_empty() {
        return;
    }
    map.insert(to_bytes_string(key), value);
}

fn clone_attr(value: &AttributeValueBytes) -> AttributeValueBytes {
    match value {
        AttributeValueBytes::String(s) => AttributeValueBytes::String(s.clone()),
        AttributeValueBytes::Float(f) => AttributeValueBytes::Float(*f),
        AttributeValueBytes::Int(i) => AttributeValueBytes::Int(*i),
        AttributeValueBytes::Bool(b) => AttributeValueBytes::Bool(*b),
        AttributeValueBytes::Bytes(b) => AttributeValueBytes::Bytes(b.clone()),
        AttributeValueBytes::KeyValue(m) => {
            let mut cloned = VecMap::with_capacity(m.len());
            for (k, v) in m.iter() {
                cloned.insert(k.clone(), clone_attr(v));
            }
            AttributeValueBytes::KeyValue(cloned)
        }
        AttributeValueBytes::List(list) => {
            AttributeValueBytes::List(list.iter().map(clone_attr).collect())
        }
    }
}

fn ddog_v1_builder_new_chunk(b: &mut TracerPayloadV1Builder, high: u64, low: u64) -> usize {
    b.push_chunk(high, low)
}

fn ddog_v1_set_chunk_sampling_priority(
    b: &mut TracerPayloadV1Builder,
    chunk: usize,
    priority: i32,
) {
    if let Some(c) = b.chunk_mut(chunk) {
        c.priority = Some(priority);
    }
}

fn ddog_v1_set_chunk_origin(b: &mut TracerPayloadV1Builder, chunk: usize, origin: CharSlice) {
    if let Some(c) = b.chunk_mut(chunk) {
        set_string_field(&mut c.origin, origin);
    }
}

fn ddog_v1_set_chunk_sampling_mechanism(
    b: &mut TracerPayloadV1Builder,
    chunk: usize,
    mechanism: u32,
) {
    if let Some(c) = b.chunk_mut(chunk) {
        c.sampling_mechanism = Some(mechanism);
    }
}

fn ddog_v1_set_chunk_dropped_trace(
    b: &mut TracerPayloadV1Builder,
    chunk: usize,
    dropped: bool,
) {
    if let Some(c) = b.chunk_mut(chunk) {
        c.dropped_trace = dropped;
    }
}

fn ddog_v1_add_chunk_attr_str(
    b: &mut TracerPayloadV1Builder,
    chunk: usize,
    key: CharSlice,
    value: CharSlice,
) {
    let value = AttributeValueBytes::String(to_bytes_string(value));
    if let Some(c) = b.chunk_mut(chunk) {
        insert_attr(&mut c.attributes, key, value);
    }
}

fn ddog_v1_chunk_new_span(b: &mut TracerPayloadV1Builder, chunk: usize) -> usize {
    b.push_span(chunk)
}

fn set_span_string(
    b: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    value: CharSlice,
    pick: impl FnOnce(&mut SpanBytes) -> &mut BytesString,
) {
    if let Some(s) = b.span_mut(chunk, span) {
        set_string_field(pick(s), value);
    }
}

fn ddog_v1_set_span_service(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.service);
}
fn ddog_v1_set_span_name(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.name);
}
fn ddog_v1_set_span_resource(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.resource);
}
fn ddog_v1_set_span_type(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.r#type);
}
fn ddog_v1_set_span_env(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.env);
}
fn ddog_v1_set_span_version(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
    set_span_string(b, c, s, v, |sp| &mut sp.version);
}
fn ddog_v1_set_span_component(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    v: CharSlice,
) {
    set_span_string(b, c, s, v, |sp| &mut sp.component);
}

fn ddog_v1_set_span_id(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: u64) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.span_id = v;
    }
}
fn ddog_v1_set_span_parent_id(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: u64) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.parent_id = v;
    }
}
fn ddog_v1_set_span_start(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: i64) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.start = v;
    }
}
fn ddog_v1_set_span_duration(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: i64) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.duration = v;
    }
}
fn ddog_v1_set_span_error(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: bool) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.error = v;
    }
}
fn ddog_v1_set_span_kind(b: &mut TracerPayloadV1Builder, c: usize, s: usize, kind: u32) {
    if let Some(sp) = b.span_mut(c, s) {
        sp.span_kind = SpanKind::from(kind);
    }
}

fn add_span_attr(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: AttributeValueBytes,
) {
    if let Some(sp) = b.span_mut(c, s) {
        insert_attr(&mut sp.attributes, key, value);
    }
}
fn ddog_v1_add_span_attr_str(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: CharSlice,
) {
    add_span_attr(
        b,
        c,
        s,
        key,
        AttributeValueBytes::String(to_bytes_string(value)),
    );
}
fn ddog_v1_add_span_attr_int(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: i64,
) {
    add_span_attr(b, c, s, key, AttributeValueBytes::Int(value));
}
fn ddog_v1_add_span_attr_double(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: f64,
) {
    add_span_attr(b, c, s, key, AttributeValueBytes::Float(value));
}
fn ddog_v1_add_span_attr_bool(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: bool,
) {
    add_span_attr(b, c, s, key, AttributeValueBytes::Bool(value));
}
fn ddog_v1_add_span_attr_bytes(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
    value: CharSlice,
) {
    let bytes = Bytes::copy_from_slice(value.as_bytes());
    add_span_attr(b, c, s, key, AttributeValueBytes::Bytes(bytes));
}

fn ddog_v1_has_span_attr(
    b: &TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
) -> bool {
    let key = to_bytes_string(key);
    b.span(c, s)
        .is_some_and(|sp| sp.attributes.contains_key(&key))
}

fn ddog_v1_del_span_attr(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    key: CharSlice,
) -> bool {
    let key = to_bytes_string(key);
    match b.span_mut(c, s) {
        Some(sp) => {
            let existed = sp.attributes.contains_key(&key);
            sp.attributes.remove_slow(&key);
            existed
        }
        None => false,
    }
}

fn ddog_v1_transfer_span_attr(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    from_span: usize,
    to_span: usize,
    key: CharSlice,
    delete_source: bool,
) -> bool {
    let key = to_bytes_string(key);
    let value = match b.span(c, from_span).and_then(|sp| sp.attributes.get(&key)) {
        Some(v) => clone_attr(v),
        None => return false,
    };
    match b.span_mut(c, to_span) {
        Some(dst) => dst.attributes.insert(key.clone(), value),
        None => return false,
    }
    if delete_source {
        if let Some(src) = b.span_mut(c, from_span) {
            src.attributes.remove_slow(&key);
        }
    }
    true
}

fn ddog_v1_span_new_link(b: &mut TracerPayloadV1Builder, c: usize, s: usize) -> usize {
    b.push_link(c, s)
}
fn ddog_v1_set_link_trace_id(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    link: usize,
    high: u64,
    low: u64,
) {
    if let Some(l) = b.link_mut(c, s, link) {
        l.trace_id = trace_id_bytes(high, low);
    }
}
fn ddog_v1_set_link_span_id(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    link: usize,
    v: u64,
) {
    if let Some(l) = b.link_mut(c, s, link) {
        l.span_id = v;
    }
}
fn ddog_v1_set_link_flags(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    link: usize,
    v: u32,
) {
    if let Some(l) = b.link_mut(c, s, link) {
        l.flags = v;
    }
}
fn ddog_v1_set_link_tracestate(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    link: usize,
    v: CharSlice,
) {
    if let Some(l) = b.link_mut(c, s, link) {
        set_string_field(&mut l.tracestate, v);
    }
}
fn ddog_v1_add_link_attr_str(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    link: usize,
    key: CharSlice,
    value: CharSlice,
) {
    let attr = AttributeValueBytes::String(to_bytes_string(value));
    if let Some(l) = b.link_mut(c, s, link) {
        insert_attr(&mut l.attributes, key, attr);
    }
}

fn ddog_v1_span_new_event(b: &mut TracerPayloadV1Builder, c: usize, s: usize) -> usize {
    b.push_event(c, s)
}
fn ddog_v1_set_event_time(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    event: usize,
    time_unix_nano: u64,
) {
    if let Some(e) = b.event_mut(c, s, event) {
        e.time_unix_nano = time_unix_nano;
    }
}
fn ddog_v1_set_event_name(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    event: usize,
    v: CharSlice,
) {
    if let Some(e) = b.event_mut(c, s, event) {
        set_string_field(&mut e.name, v);
    }
}
fn ddog_v1_add_event_attr_int(
    b: &mut TracerPayloadV1Builder,
    c: usize,
    s: usize,
    event: usize,
    key: CharSlice,
    value: i64,
) {
    if let Some(e) = b.event_mut(c, s, event) {
        insert_attr(&mut e.attributes, key, AttributeValueBytes::Int(value));
    }
}

#[test]
fn builds_span_with_promoted_and_typed_attributes() {
    let mut b = TracerPayloadV1Builder::default();

    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 0x0123456789abcdef);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("svc"));
    ddog_v1_set_span_name(&mut b, ci, si, cs("op"));
    ddog_v1_set_span_resource(&mut b, ci, si, cs("res"));
    ddog_v1_set_span_id(&mut b, ci, si, 42);
    ddog_v1_set_span_start(&mut b, ci, si, 1_000);
    ddog_v1_set_span_duration(&mut b, ci, si, 500);
    ddog_v1_set_span_error(&mut b, ci, si, true);
    ddog_v1_set_span_kind(&mut b, ci, si, 2); // Server
    ddog_v1_add_span_attr_str(&mut b, ci, si, cs("k_str"), cs("v_str"));
    ddog_v1_add_span_attr_int(&mut b, ci, si, cs("k_int"), 7);

    let payload = b.into_payload();
    let encoded = to_vec_from_v1(&payload);

    // trace_id big-endian 16 bytes present
    let expected_tid = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
        0xcd, 0xef,
    ];
    assert!(encoded.windows(16).any(|w| w == expected_tid));
    for s in &[b"svc" as &[u8], b"op", b"res", b"k_str", b"v_str", b"k_int"] {
        assert!(
            encoded.windows(s.len()).any(|w| w == *s),
            "{} should appear",
            std::str::from_utf8(s).unwrap()
        );
    }
    // SpanKind Server = 2: key 16 (0x10) then uint 2 (0x02)
    assert!(encoded.windows(2).any(|w| w == [0x10, 0x02]));
}

#[test]
fn getters_round_trip_setters() {
    let mut b = TracerPayloadV1Builder::default();

    let ci = ddog_v1_builder_new_chunk(&mut b, 0xaabb, 0xccdd);
    ddog_v1_set_chunk_sampling_priority(&mut b, ci, 2);
    ddog_v1_set_chunk_origin(&mut b, ci, cs("lambda"));
    ddog_v1_set_chunk_sampling_mechanism(&mut b, ci, 4);
    ddog_v1_set_chunk_dropped_trace(&mut b, ci, true);
    ddog_v1_add_chunk_attr_str(&mut b, ci, cs("c_key"), cs("c_val"));

    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("svc"));
    ddog_v1_set_span_name(&mut b, ci, si, cs("op"));
    ddog_v1_set_span_resource(&mut b, ci, si, cs("res"));
    ddog_v1_set_span_type(&mut b, ci, si, cs("web"));
    ddog_v1_set_span_env(&mut b, ci, si, cs("prod"));
    ddog_v1_set_span_version(&mut b, ci, si, cs("1.2.3"));
    ddog_v1_set_span_component(&mut b, ci, si, cs("pdo"));
    ddog_v1_set_span_id(&mut b, ci, si, 42);
    ddog_v1_set_span_parent_id(&mut b, ci, si, 7);
    ddog_v1_set_span_start(&mut b, ci, si, 1_000);
    ddog_v1_set_span_duration(&mut b, ci, si, 500);
    ddog_v1_set_span_error(&mut b, ci, si, true);
    ddog_v1_set_span_kind(&mut b, ci, si, 3); // Client
    ddog_v1_add_span_attr_str(&mut b, ci, si, cs("a_str"), cs("v"));
    ddog_v1_add_span_attr_int(&mut b, ci, si, cs("a_int"), 11);
    ddog_v1_add_span_attr_double(&mut b, ci, si, cs("a_dbl"), 1.5);
    ddog_v1_add_span_attr_bool(&mut b, ci, si, cs("a_bool"), true);
    ddog_v1_add_span_attr_bytes(&mut b, ci, si, cs("a_bytes"), cs("raw"));

    let li = ddog_v1_span_new_link(&mut b, ci, si);
    ddog_v1_set_link_trace_id(&mut b, ci, si, li, 0x11, 0x22);
    ddog_v1_set_link_span_id(&mut b, ci, si, li, 9);
    ddog_v1_set_link_flags(&mut b, ci, si, li, 1);
    ddog_v1_set_link_tracestate(&mut b, ci, si, li, cs("dd=s:1"));
    ddog_v1_add_link_attr_str(&mut b, ci, si, li, cs("l_key"), cs("l_val"));

    let evi = ddog_v1_span_new_event(&mut b, ci, si);
    ddog_v1_set_event_time(&mut b, ci, si, evi, 123);
    ddog_v1_set_event_name(&mut b, ci, si, evi, cs("exception"));
    ddog_v1_add_event_attr_int(&mut b, ci, si, evi, cs("e_int"), 5);

    // Chunk getters.
    assert_eq!(ddog_v1_get_chunk_count(&b), 1);
    assert_eq!(ddog_v1_get_chunk_trace_id_high(&b, ci), 0xaabb);
    assert_eq!(ddog_v1_get_chunk_trace_id_low(&b, ci), 0xccdd);
    let mut prio = 0;
    assert!(ddog_v1_get_chunk_sampling_priority(&b, ci, &mut prio));
    assert_eq!(prio, 2);
    let mut mech = 0;
    assert!(ddog_v1_get_chunk_sampling_mechanism(&b, ci, &mut mech));
    assert_eq!(mech, 4);
    assert_eq!(ddog_v1_get_chunk_origin(&b, ci).to_utf8_lossy(), "lambda");
    assert!(ddog_v1_get_chunk_dropped_trace(&b, ci));
    assert_eq!(ddog_v1_get_chunk_attr_count(&b, ci), 1);
    assert_eq!(
        ddog_v1_get_chunk_attr_key(&b, ci, 0).to_utf8_lossy(),
        "c_key"
    );
    assert_eq!(ddog_v1_get_chunk_attr_type(&b, ci, 0), DDOG_V1_ATTR_STRING);
    assert_eq!(
        ddog_v1_get_chunk_attr_str(&b, ci, 0).to_utf8_lossy(),
        "c_val"
    );

    // Span getters.
    assert_eq!(ddog_v1_get_span_count(&b, ci), 1);
    assert_eq!(ddog_v1_get_span_service(&b, ci, si).to_utf8_lossy(), "svc");
    assert_eq!(ddog_v1_get_span_name(&b, ci, si).to_utf8_lossy(), "op");
    assert_eq!(ddog_v1_get_span_resource(&b, ci, si).to_utf8_lossy(), "res");
    assert_eq!(ddog_v1_get_span_type(&b, ci, si).to_utf8_lossy(), "web");
    assert_eq!(ddog_v1_get_span_env(&b, ci, si).to_utf8_lossy(), "prod");
    assert_eq!(
        ddog_v1_get_span_version(&b, ci, si).to_utf8_lossy(),
        "1.2.3"
    );
    assert_eq!(
        ddog_v1_get_span_component(&b, ci, si).to_utf8_lossy(),
        "pdo"
    );
    assert_eq!(ddog_v1_get_span_id(&b, ci, si), 42);
    assert_eq!(ddog_v1_get_span_parent_id(&b, ci, si), 7);
    assert_eq!(ddog_v1_get_span_start(&b, ci, si), 1_000);
    assert_eq!(ddog_v1_get_span_duration(&b, ci, si), 500);
    assert!(ddog_v1_get_span_error(&b, ci, si));
    assert_eq!(ddog_v1_get_span_kind(&b, ci, si), 3);

    // Span attributes: 5 typed values in insertion order.
    assert_eq!(ddog_v1_get_span_attr_count(&b, ci, si), 5);
    assert_eq!(
        ddog_v1_get_span_attr_key(&b, ci, si, 0).to_utf8_lossy(),
        "a_str"
    );
    assert_eq!(
        ddog_v1_get_span_attr_type(&b, ci, si, 0),
        DDOG_V1_ATTR_STRING
    );
    assert_eq!(
        ddog_v1_get_span_attr_str(&b, ci, si, 0).to_utf8_lossy(),
        "v"
    );
    assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 1), DDOG_V1_ATTR_INT);
    assert_eq!(ddog_v1_get_span_attr_int(&b, ci, si, 1), 11);
    assert_eq!(
        ddog_v1_get_span_attr_type(&b, ci, si, 2),
        DDOG_V1_ATTR_DOUBLE
    );
    assert_eq!(ddog_v1_get_span_attr_double(&b, ci, si, 2), 1.5);
    assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 3), DDOG_V1_ATTR_BOOL);
    assert!(ddog_v1_get_span_attr_bool(&b, ci, si, 3));
    assert_eq!(
        ddog_v1_get_span_attr_type(&b, ci, si, 4),
        DDOG_V1_ATTR_BYTES
    );
    assert_eq!(
        ddog_v1_get_span_attr_bytes(&b, ci, si, 4).to_utf8_lossy(),
        "raw"
    );

    // Link getters.
    assert_eq!(ddog_v1_get_link_count(&b, ci, si), 1);
    assert_eq!(ddog_v1_get_link_trace_id_high(&b, ci, si, li), 0x11);
    assert_eq!(ddog_v1_get_link_trace_id_low(&b, ci, si, li), 0x22);
    assert_eq!(ddog_v1_get_link_span_id(&b, ci, si, li), 9);
    assert_eq!(ddog_v1_get_link_flags(&b, ci, si, li), 1);
    assert_eq!(
        ddog_v1_get_link_tracestate(&b, ci, si, li).to_utf8_lossy(),
        "dd=s:1"
    );
    assert_eq!(ddog_v1_get_link_attr_count(&b, ci, si, li), 1);
    assert_eq!(
        ddog_v1_get_link_attr_key(&b, ci, si, li, 0).to_utf8_lossy(),
        "l_key"
    );
    assert_eq!(
        ddog_v1_get_link_attr_str(&b, ci, si, li, 0).to_utf8_lossy(),
        "l_val"
    );

    // Event getters.
    assert_eq!(ddog_v1_get_event_count(&b, ci, si), 1);
    assert_eq!(ddog_v1_get_event_time(&b, ci, si, evi), 123);
    assert_eq!(
        ddog_v1_get_event_name(&b, ci, si, evi).to_utf8_lossy(),
        "exception"
    );
    assert_eq!(ddog_v1_get_event_attr_count(&b, ci, si, evi), 1);
    assert_eq!(
        ddog_v1_get_event_attr_key(&b, ci, si, evi, 0).to_utf8_lossy(),
        "e_int"
    );
    assert_eq!(
        ddog_v1_get_event_attr_type(&b, ci, si, evi, 0),
        DDOG_V1_ATTR_INT
    );
    assert_eq!(ddog_v1_get_event_attr_int(&b, ci, si, evi, 0), 5);

    // Out-of-range access is safe and returns defaults.
    assert_eq!(ddog_v1_get_span_service(&b, 99, 0).to_utf8_lossy(), "");
    assert_eq!(ddog_v1_get_span_id(&b, 0, 99), 0);
}

#[test]
fn encoder_streams_repeated_string_once() {
    // "shared" used as service in two chunks must appear as raw bytes exactly once on the
    // wire: the encoder's streaming string table emits the second occurrence as a uint id.
    let mut b = TracerPayloadV1Builder::default();

    let c0 = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    let s0 = ddog_v1_chunk_new_span(&mut b, c0);
    ddog_v1_set_span_service(&mut b, c0, s0, cs("shared"));
    ddog_v1_set_span_name(&mut b, c0, s0, cs("op1"));
    ddog_v1_set_span_id(&mut b, c0, s0, 1);

    let c1 = ddog_v1_builder_new_chunk(&mut b, 0, 2);
    let s1 = ddog_v1_chunk_new_span(&mut b, c1);
    ddog_v1_set_span_service(&mut b, c1, s1, cs("shared"));
    ddog_v1_set_span_name(&mut b, c1, s1, cs("op2"));
    ddog_v1_set_span_id(&mut b, c1, s1, 2);

    let encoded = to_vec_from_v1(&b.into_payload());
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
    let mut b = TracerPayloadV1Builder::default();

    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("svc"));
    ddog_v1_set_span_id(&mut b, ci, si, 1);

    let li = ddog_v1_span_new_link(&mut b, ci, si);
    ddog_v1_set_link_trace_id(&mut b, ci, si, li, 0xaa, 0xbb);
    ddog_v1_set_link_span_id(&mut b, ci, si, li, 9);
    ddog_v1_set_link_flags(&mut b, ci, si, li, 1);
    ddog_v1_set_link_tracestate(&mut b, ci, si, li, cs("dd=s:1"));
    ddog_v1_add_link_attr_str(&mut b, ci, si, li, cs("link.attr"), cs("link.val"));

    let evi = ddog_v1_span_new_event(&mut b, ci, si);
    ddog_v1_set_event_time(&mut b, ci, si, evi, 123);
    ddog_v1_set_event_name(&mut b, ci, si, evi, cs("exception"));
    ddog_v1_add_event_attr_int(&mut b, ci, si, evi, cs("ev.attr"), 5);

    let encoded = to_vec_from_v1(&b.into_payload());
    for s in &[
        b"exception" as &[u8],
        b"dd=s:1",
        b"link.attr",
        b"link.val",
        b"ev.attr",
    ] {
        assert!(
            encoded.windows(s.len()).any(|w| w == *s),
            "{} should appear",
            std::str::from_utf8(s).unwrap()
        );
    }
    // link trace_id low half 0xbb present in the 16-byte BE id
    let expected_link_tid = [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xbb,
    ];
    assert!(encoded.windows(16).any(|w| w == expected_link_tid));
}

#[test]
fn chunk_level_fields_encoded() {
    let mut b = TracerPayloadV1Builder::default();
    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    ddog_v1_set_chunk_sampling_priority(&mut b, ci, 2);
    ddog_v1_set_chunk_origin(&mut b, ci, cs("lambda"));
    ddog_v1_set_chunk_sampling_mechanism(&mut b, ci, 4);
    ddog_v1_set_chunk_dropped_trace(&mut b, ci, true);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("svc"));
    ddog_v1_set_span_id(&mut b, ci, si, 1);

    let encoded = to_vec_from_v1(&b.into_payload());
    assert!(encoded.windows(b"lambda".len()).any(|w| w == b"lambda"));
    // sampling_mechanism = 4 (chunk key 0x07 + fixint 0x04)
    assert!(encoded.windows(2).any(|w| w == [0x07, 0x04]));
    // dropped_trace true (chunk key 0x05 + msgpack true 0xc3)
    assert!(encoded.windows(2).any(|w| w == [0x05, 0xc3]));
}

#[test]
fn populate_metadata_sets_container_id_and_fields() {
    let mut b = TracerPayloadV1Builder::default();
    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("svc"));
    ddog_v1_set_span_id(&mut b, ci, si, 1);
    let mut payload = b.into_payload();

    populate_payload_metadata(
        &mut payload,
        "container-xyz",
        "php",
        "8.3",
        "1.2.3",
        "runtime-uuid",
        "prod",
        "my-host",
        "4.5.6",
        "deadbeef",
    );
    let encoded = to_vec_from_v1(&payload);
    for s in &[
        b"container-xyz" as &[u8],
        b"php",
        b"8.3",
        b"1.2.3",
        b"runtime-uuid",
        b"prod",
        b"my-host",
        b"4.5.6",
        b"deadbeef",
        b"_dd.git.commit.sha",
    ] {
        assert!(
            encoded.windows(s.len()).any(|w| w == *s),
            "{} should appear in payload (container_id must not be dropped)",
            std::str::from_utf8(s).unwrap()
        );
    }
}

#[test]
fn has_and_del_span_attr_round_trip() {
    let mut b = TracerPayloadV1Builder::default();
    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_add_span_attr_str(&mut b, ci, si, cs("error.ignored"), cs("1"));
    ddog_v1_add_span_attr_int(&mut b, ci, si, cs("keep"), 7);

    // has: present vs absent.
    assert!(ddog_v1_has_span_attr(&b, ci, si, cs("error.ignored")));
    assert!(ddog_v1_has_span_attr(&b, ci, si, cs("keep")));
    assert!(!ddog_v1_has_span_attr(&b, ci, si, cs("missing")));
    // out-of-range span is safe.
    assert!(!ddog_v1_has_span_attr(&b, ci, 99, cs("keep")));

    // del: returns whether it existed and actually removes it.
    assert!(ddog_v1_del_span_attr(&mut b, ci, si, cs("error.ignored")));
    assert!(!ddog_v1_has_span_attr(&b, ci, si, cs("error.ignored")));
    // deleting again reports absence.
    assert!(!ddog_v1_del_span_attr(&mut b, ci, si, cs("error.ignored")));
    // untouched sibling survives.
    assert!(ddog_v1_has_span_attr(&b, ci, si, cs("keep")));
    assert_eq!(ddog_v1_get_span_attr_count(&b, ci, si), 1);
}

#[test]
fn transfer_span_attr_copies_and_optionally_deletes() {
    let mut b = TracerPayloadV1Builder::default();
    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
    let root = ddog_v1_chunk_new_span(&mut b, ci);
    let inferred = ddog_v1_chunk_new_span(&mut b, ci);

    // A string meta-like attr (copy, keep source) and a numeric metric-like attr (move).
    ddog_v1_add_span_attr_str(&mut b, ci, root, cs("error.message"), cs("boom"));
    ddog_v1_add_span_attr_double(&mut b, ci, root, cs("_dd.agent_psr"), 0.5);

    // meta copy without deleting the source (mirrors delete_source=false).
    assert!(ddog_v1_transfer_span_attr(
        &mut b,
        ci,
        root,
        inferred,
        cs("error.message"),
        false
    ));
    assert!(ddog_v1_has_span_attr(&b, ci, root, cs("error.message")));
    assert!(ddog_v1_has_span_attr(&b, ci, inferred, cs("error.message")));
    // value is preserved (String, "boom") on the destination.
    assert_eq!(ddog_v1_get_span_attr_count(&b, ci, inferred), 1);
    assert_eq!(
        ddog_v1_get_span_attr_key(&b, ci, inferred, 0).to_utf8_lossy(),
        "error.message"
    );
    assert_eq!(
        ddog_v1_get_span_attr_type(&b, ci, inferred, 0),
        DDOG_V1_ATTR_STRING
    );
    assert_eq!(
        ddog_v1_get_span_attr_str(&b, ci, inferred, 0).to_utf8_lossy(),
        "boom"
    );

    // metric move with deletion of the source (mirrors delete_source=true).
    assert!(ddog_v1_transfer_span_attr(
        &mut b,
        ci,
        root,
        inferred,
        cs("_dd.agent_psr"),
        true
    ));
    assert!(!ddog_v1_has_span_attr(&b, ci, root, cs("_dd.agent_psr")));
    assert!(ddog_v1_has_span_attr(&b, ci, inferred, cs("_dd.agent_psr")));
    assert_eq!(
        ddog_v1_get_span_attr_type(&b, ci, inferred, 1),
        DDOG_V1_ATTR_DOUBLE
    );
    assert_eq!(ddog_v1_get_span_attr_double(&b, ci, inferred, 1), 0.5);

    // absent source key is a no-op (destination untouched, nothing deleted).
    assert!(!ddog_v1_transfer_span_attr(
        &mut b,
        ci,
        root,
        inferred,
        cs("missing"),
        true
    ));
    assert_eq!(ddog_v1_get_span_attr_count(&b, ci, inferred), 2);

    // encode still yields a valid, non-empty V1 payload with the transferred keys present.
    ddog_v1_set_span_id(&mut b, ci, root, 1);
    ddog_v1_set_span_id(&mut b, ci, inferred, 2);
    let encoded = to_vec_from_v1(&b.into_payload());
    assert!(!encoded.is_empty());
    for s in &[b"error.message" as &[u8], b"boom", b"_dd.agent_psr"] {
        assert!(
            encoded.windows(s.len()).any(|w| w == *s),
            "{} should appear",
            std::str::from_utf8(s).unwrap()
        );
    }
}

#[test]
fn span_debug_log_renders_readable_string() {
    let mut b = TracerPayloadV1Builder::default();
    let ci = ddog_v1_builder_new_chunk(&mut b, 0, 0xdead);
    let si = ddog_v1_chunk_new_span(&mut b, ci);
    ddog_v1_set_span_service(&mut b, ci, si, cs("my-service"));
    ddog_v1_set_span_name(&mut b, ci, si, cs("my-operation"));
    ddog_v1_set_span_resource(&mut b, ci, si, cs("GET /x"));
    ddog_v1_set_span_id(&mut b, ci, si, 42);
    ddog_v1_set_span_parent_id(&mut b, ci, si, 7);
    ddog_v1_set_span_start(&mut b, ci, si, 1_000);
    ddog_v1_set_span_duration(&mut b, ci, si, 500);
    ddog_v1_set_span_error(&mut b, ci, si, true);
    ddog_v1_set_span_kind(&mut b, ci, si, 2); // Server
    ddog_v1_set_span_component(&mut b, ci, si, cs("pdo"));
    ddog_v1_add_span_attr_int(&mut b, ci, si, cs("http.status_code"), 200);
    let _ = ddog_v1_span_new_link(&mut b, ci, si);
    let _ = ddog_v1_span_new_event(&mut b, ci, si);

    let slice = ddog_v1_span_debug_log(&b, ci, si);
    let rendered = slice.to_utf8_lossy().to_string();
    assert!(!rendered.is_empty());
    assert!(
        rendered.contains("my-service"),
        "service should appear: {rendered}"
    );
    assert!(
        rendered.contains("my-operation"),
        "name should appear: {rendered}"
    );
    assert!(
        rendered.contains("resource=\"GET /x\""),
        "resource should appear: {rendered}"
    );
    assert!(rendered.contains("span_id=42"));
    assert!(rendered.contains("parent_id=7"));
    assert!(rendered.contains("error=true"));
    assert!(rendered.contains("kind=Server"));
    assert!(rendered.contains("component=\"pdo\""));
    assert!(rendered.contains("http.status_code=200"));
    assert!(rendered.contains("links=1"));
    assert!(rendered.contains("events=1"));
    // Frees correctly via the same free function as the v0.4 variant.
    unsafe { ddog_free_charslice(slice) };

    // Out-of-range index yields an empty (safely freeable) slice.
    let empty = ddog_v1_span_debug_log(&b, 99, 0);
    assert!(empty.to_utf8_lossy().is_empty());
    unsafe { ddog_free_charslice(empty) };
}
