// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI builder for the native V1 trace payload ([`TracerPayloadBytes`]), storing readable
//! [`BytesString`]s directly. As with the v0.4 `TracesBytes`, C gets plain pointers into the model:
//! a chunk/span/link/event pointer (and any [`Attributes`] handle derived from it) is invalidated
//! by the next push into the same parent; re-fetch it by index (e.g. [`ddog_v1_get_span`]). Payload
//! env / app_version / hostname are set on the builder; the rest is applied at send time (see
//! [`populate_payload_metadata`]).

use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_utils::span::v1::{
    AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes, TraceChunkBytes,
    TracerPayloadBytes,
};
use libdd_trace_utils::span::vec_map::VecMap;
use std::borrow::Cow;
use std::ffi::{CStr, CString, c_char};
use std::fmt::Write as _;
use std::ptr;

/// Attribute value type tags from [`ddog_v1_value_type`], so a C caller picks the matching typed
/// value getter.
pub const DDOG_V1_ATTR_STRING: u32 = 0;
pub const DDOG_V1_ATTR_INT: u32 = 1;
pub const DDOG_V1_ATTR_DOUBLE: u32 = 2;
pub const DDOG_V1_ATTR_BOOL: u32 = 3;
pub const DDOG_V1_ATTR_BYTES: u32 = 4;
pub const DDOG_V1_ATTR_KEYVALUE: u32 = 5;
pub const DDOG_V1_ATTR_LIST: u32 = 6;

type AttrVecMap = VecMap<BytesString, AttributeValueBytes>;

/// Composes a 128-bit trace id from its high/low 64-bit halves into 16 big-endian bytes.
fn trace_id_bytes(high: u64, low: u64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&high.to_be_bytes());
    bytes[8..].copy_from_slice(&low.to_be_bytes());
    bytes
}

/// High 64 bits of a 16-byte big-endian trace id.
fn trace_id_high(bytes: &[u8; 16]) -> u64 {
    let mut half = [0u8; 8];
    half.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(half)
}

/// Low 64 bits of a 16-byte big-endian trace id.
fn trace_id_low(bytes: &[u8; 16]) -> u64 {
    let mut half = [0u8; 8];
    half.copy_from_slice(&bytes[8..]);
    u64::from_be_bytes(half)
}

/// Borrows a stored `BytesString` as a `CharSlice`.
#[inline]
fn char_slice_of<'a>(field: &BytesString) -> CharSlice<'a> {
    let s = field.as_str();
    // Safety: `BytesString` guarantees valid UTF-8; the slice borrows `field`, which the caller
    // keeps alive and unmodified while C reads it.
    unsafe { CharSlice::from_raw_parts(s.as_ptr().cast(), s.len()) }
}

/// Copies a `CharSlice` into a `BytesString`, replacing invalid UTF-8 (lossy).
fn bytes_string_from_slice(slice: CharSlice) -> BytesString {
    match String::from_utf8_lossy(slice.as_bytes()) {
        Cow::Owned(s) => s.into(),
        // Safety: `from_utf8_lossy` borrowed, so the bytes are valid UTF-8.
        Cow::Borrowed(_) => unsafe {
            BytesString::from_bytes_unchecked(slice.as_bytes().to_vec().into())
        },
    }
}

/// Wraps a static NUL-terminated C string literal without copying it (lossy for invalid UTF-8).
///
/// # Safety
/// `string` must point to a NUL-terminated string that lives for the rest of the program.
unsafe fn bytes_string_from_literal(string: *const c_char) -> BytesString {
    // Safety: per the fn contract.
    let cstring: &'static CStr = unsafe { CStr::from_ptr(string) };
    match String::from_utf8_lossy(cstring.to_bytes()) {
        Cow::Owned(s) => s.into(),
        Cow::Borrowed(s) => BytesString::from_static(s),
    }
}

/// Sets a string field, leaving it unchanged for an empty slice so absent values stay unset.
#[inline]
fn set_field(field: &mut BytesString, value: CharSlice) {
    if !value.is_empty() {
        *field = bytes_string_from_slice(value);
    }
}

/// Deep-clones an attribute value (`AttributeValue` can't derive `Clone`).
fn clone_attr(value: &AttributeValueBytes) -> AttributeValueBytes {
    match value {
        AttributeValueBytes::String(s) => AttributeValueBytes::String(s.clone()),
        AttributeValueBytes::Float(f) => AttributeValueBytes::Float(*f),
        AttributeValueBytes::Int(i) => AttributeValueBytes::Int(*i),
        AttributeValueBytes::Bool(b) => AttributeValueBytes::Bool(*b),
        AttributeValueBytes::Bytes(b) => AttributeValueBytes::Bytes(b.clone()),
        AttributeValueBytes::KeyValue(m) => AttributeValueBytes::KeyValue(
            m.iter().map(|(k, v)| (k.clone(), clone_attr(v))).collect(),
        ),
        AttributeValueBytes::List(list) => {
            AttributeValueBytes::List(list.iter().map(clone_attr).collect())
        }
    }
}

// ------------------- Builder lifecycle -------------------

/// Creates a new, empty V1 payload builder. Free it with [`ddog_v1_free_builder`], or hand it to
/// `ddog_send_traces_to_sidecar_v1`, which consumes it.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_new_builder() -> Box<TracerPayloadBytes> {
    Box::default()
}

/// Frees a V1 payload builder.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_free_builder(_builder: Box<TracerPayloadBytes>) {}

/// Sets the payload `env` / `app_version` / `hostname` fields (empty = unset).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_payload_metadata(
    builder: &mut TracerPayloadBytes,
    env: CharSlice,
    app_version: CharSlice,
    hostname: CharSlice,
) {
    builder.env = bytes_string_from_slice(env);
    builder.app_version = bytes_string_from_slice(app_version);
    builder.hostname = bytes_string_from_slice(hostname);
}

// ------------------- Chunk / span / link / event creation -------------------
//
// Handles are taken as references; creators and index getters return raw pointers (null if out of
// range). A returned `&mut` would be invalidated (Stacked Borrows) by the next reborrow of the same
// element, e.g. re-fetching it by index, while C keeps the earlier pointer.

/// Pushes `item`, returning a pointer to it. `as_mut_ptr` materializes no reference to the other
/// elements, so pointers to them stay valid unless the push reallocates.
fn push_item<T>(vec: &mut Vec<T>, item: T) -> *mut T {
    vec.push(item);
    // Safety: `len - 1` is in bounds after the push.
    unsafe { vec.as_mut_ptr().add(vec.len() - 1) }
}

/// Pointer to element `idx` (null if out of range), without materializing a reference to the
/// others.
fn item_at<T>(vec: &mut Vec<T>, idx: usize) -> *mut T {
    if idx < vec.len() {
        // Safety: in bounds.
        unsafe { vec.as_mut_ptr().add(idx) }
    } else {
        ptr::null_mut()
    }
}

/// Appends a chunk carrying the 128-bit trace id (high/low halves).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_new_chunk(
    builder: &mut TracerPayloadBytes,
    trace_id_high: u64,
    trace_id_low: u64,
) -> *mut TraceChunkBytes {
    let chunk = TraceChunkBytes {
        trace_id: trace_id_bytes(trace_id_high, trace_id_low),
        ..Default::default()
    };
    push_item(&mut builder.chunks, chunk)
}

/// Number of spans already in `chunk`.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_chunk_span_count(chunk: &TraceChunkBytes) -> usize {
    chunk.spans.len()
}

/// Appends an empty span to `chunk`.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_new_span(chunk: &mut TraceChunkBytes) -> *mut SpanBytes {
    push_item(&mut chunk.spans, SpanBytes::default())
}

/// Appends an empty link to `span`.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_new_link(span: &mut SpanBytes) -> *mut SpanLinkBytes {
    span.span_links.push(SpanLinkBytes::default());
    span.span_links
        .last_mut()
        .map_or(ptr::null_mut(), ptr::from_mut)
}

/// Appends an empty event to `span`.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_new_event(span: &mut SpanBytes) -> *mut SpanEventBytes {
    span.span_events.push(SpanEventBytes::default());
    span.span_events
        .last_mut()
        .map_or(ptr::null_mut(), ptr::from_mut)
}

// ------------------- Span fields -------------------

#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_set_id(span: &mut SpanBytes, value: u64) {
    span.span_id = value;
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_set_parent_id(span: &mut SpanBytes, value: u64) {
    span.parent_id = value;
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_set_start(span: &mut SpanBytes, value: i64) {
    span.start = value;
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_set_duration(span: &mut SpanBytes, value: i64) {
    span.duration = value;
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_set_error(span: &mut SpanBytes, error: bool) {
    span.error = error;
}

/// Reads the span error flag (e.g. to mirror it onto an inferred span).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_span_get_error(span: &SpanBytes) -> bool {
    span.error
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_service(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.service, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_name(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.name, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_resource(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.resource, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_type(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.r#type, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_env(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.env, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_version(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.version, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_component(span: &mut SpanBytes, value: CharSlice) {
    set_field(&mut span.component, value);
}

/// Sets the span kind from an OTEL wire value (unset/unknown → Unspecified).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_span_kind(span: &mut SpanBytes, kind: u32) {
    span.span_kind = SpanKind::from(kind);
}

// ------------------- Chunk fields -------------------

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_chunk_origin(chunk: &mut TraceChunkBytes, origin: CharSlice) {
    set_field(&mut chunk.origin, origin);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_chunk_sampling_priority(chunk: &mut TraceChunkBytes, priority: i32) {
    chunk.priority = Some(priority);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_chunk_sampling_mechanism(chunk: &mut TraceChunkBytes, mechanism: u32) {
    chunk.sampling_mechanism = Some(mechanism);
}

// ------------------- Link / event fields -------------------

#[unsafe(no_mangle)]
pub extern "C" fn ddog_link_set_trace_id(
    link: &mut SpanLinkBytes,
    trace_id_high: u64,
    trace_id_low: u64,
) {
    link.trace_id = trace_id_bytes(trace_id_high, trace_id_low);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_link_set_span_id(link: &mut SpanLinkBytes, value: u64) {
    link.span_id = value;
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_link_set_tracestate(link: &mut SpanLinkBytes, value: CharSlice) {
    set_field(&mut link.tracestate, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_event_set_name(event: &mut SpanEventBytes, value: CharSlice) {
    set_field(&mut event.name, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_event_set_time(event: &mut SpanEventBytes, time_unix_nano: u64) {
    event.time_unix_nano = time_unix_nano;
}

// ------------------- Attributes -------------------
//
// Every attribute map is filled through the `ddog_attributes_add_*` family and read through the
// `ddog_v1_attributes_*` family, on an `Attributes` handle. The payload/chunk/span/link/event
// getters take a raw pointer: the handle is a place projection off it (no intermediate `&mut`), so
// it stays valid across later `&mut` calls on the same element (Stacked Borrows).

/// An attribute map: the payload's, a chunk's, span's, link's or event's (`ddog_*_get_attributes`),
/// or an owned nested map's ([`ddog_attr_map_get_attributes`]). Opaque to C.
#[repr(transparent)]
pub struct Attributes(pub AttrVecMap);

#[inline]
fn attributes_handle(map: *mut AttrVecMap) -> *mut Attributes {
    map.cast()
}

/// The payload's attribute map.
///
/// # Safety
/// `builder` must be a live builder from [`ddog_v1_new_builder`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_payload_get_attributes(
    builder: *mut TracerPayloadBytes,
) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*builder).attributes })
}

/// # Safety
/// `chunk` must be a live pointer from [`ddog_new_chunk`] or [`ddog_v1_get_chunk`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_chunk_get_attributes(chunk: *mut TraceChunkBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*chunk).attributes })
}

/// # Safety
/// `span` must be a live pointer from [`ddog_new_span`] or [`ddog_v1_get_span`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_get_attributes(span: *mut SpanBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*span).attributes })
}

/// # Safety
/// `link` must be a live pointer from [`ddog_new_link`] or [`ddog_v1_get_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_get_attributes(link: *mut SpanLinkBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*link).attributes })
}

/// # Safety
/// `event` must be a live pointer from [`ddog_new_event`] or [`ddog_v1_get_event`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_event_get_attributes(event: *mut SpanEventBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*event).attributes })
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_str(
    attrs: &mut Attributes,
    key: CharSlice,
    value: CharSlice,
) {
    let value = AttributeValueBytes::String(bytes_string_from_slice(value));
    attrs.0.insert(bytes_string_from_slice(key), value);
}

/// String attribute under a static C literal key (not copied).
///
/// # Safety
/// `key` must be a NUL-terminated string that lives for the rest of the program.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_lit(
    attrs: &mut Attributes,
    key: *const c_char,
    value: CharSlice,
) {
    let value = AttributeValueBytes::String(bytes_string_from_slice(value));
    attrs
        .0
        .insert(unsafe { bytes_string_from_literal(key) }, value);
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_int(attrs: &mut Attributes, key: CharSlice, value: i64) {
    attrs.0.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Int(value),
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_double(attrs: &mut Attributes, key: CharSlice, value: f64) {
    attrs.0.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Float(value),
    );
}

/// Double attribute under a static C literal key (not copied).
///
/// # Safety
/// See [`ddog_attributes_add_lit`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_double_lit(
    attrs: &mut Attributes,
    key: *const c_char,
    value: f64,
) {
    let key = unsafe { bytes_string_from_literal(key) };
    attrs.0.insert(key, AttributeValueBytes::Float(value));
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_bool(attrs: &mut Attributes, key: CharSlice, value: bool) {
    attrs.0.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Bool(value),
    );
}

/// Bytes attribute (v0.4 `meta_struct`), copied verbatim.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_bytes(
    attrs: &mut Attributes,
    key: CharSlice,
    value: CharSlice,
) {
    let value = AttributeValueBytes::Bytes(Bytes::copy_from_slice(value.as_bytes()));
    attrs.0.insert(bytes_string_from_slice(key), value);
}

/// Sets `attrs[key]` to `list`, which is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_list(
    attrs: &mut Attributes,
    key: CharSlice,
    list: Box<AttrList>,
) {
    attrs.0.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::List(list.0),
    );
}

/// Sets `attrs[key]` to `map`, which is consumed (it must not be the map `attrs` belongs to).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attributes_add_map(
    attrs: &mut Attributes,
    key: CharSlice,
    map: Box<AttrMap>,
) {
    attrs.0.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::KeyValue(map.0),
    );
}

/// Copies the attribute `key` from `from_span` onto `to_span`, returning whether the source had it;
/// removes it from the source when `delete_source` is set. Type-preserving.
///
/// # Safety
/// `from_span`/`to_span` must be distinct; `key` a static NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_transfer_span_attr(
    from_span: &mut SpanBytes,
    to_span: &mut SpanBytes,
    key: *const c_char,
    delete_source: bool,
) -> bool {
    let key = unsafe { bytes_string_from_literal(key) };
    let value = match from_span.attributes.get(&key) {
        Some(v) => clone_attr(v),
        None => return false,
    };
    to_span.attributes.insert(key.clone(), value);
    if delete_source {
        from_span.attributes.remove_slow(&key);
    }
    true
}

// ------------------- Nested attributes: owned containers -------------------
//
// C builds a nested value bottom-up: allocate a list/map, fill it, push it into its parent (which
// takes ownership), and finally add the outermost container to an attribute map.

/// An owned `List` attribute value under construction. Opaque to C.
pub struct AttrList(Vec<AttributeValueBytes>);

/// An owned `KeyValue` attribute value under construction. Opaque to C.
pub struct AttrMap(AttrVecMap);

/// Allocates an empty list with room for `capacity` elements, owned by C until it is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_new(capacity: usize) -> Box<AttrList> {
    Box::new(AttrList(Vec::with_capacity(capacity)))
}

/// Allocates an empty map with room for `capacity` members, owned by C until it is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_map_new(capacity: usize) -> Box<AttrMap> {
    Box::new(AttrMap(VecMap::with_capacity(capacity)))
}

/// The attribute map of an owned nested `map`, filled with the `ddog_attributes_add_*` family.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_map_get_attributes(map: &mut AttrMap) -> *mut Attributes {
    attributes_handle(&raw mut map.0)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_str(list: &mut AttrList, value: CharSlice) {
    list.0
        .push(AttributeValueBytes::String(bytes_string_from_slice(value)));
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_int(list: &mut AttrList, value: i64) {
    list.0.push(AttributeValueBytes::Int(value));
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_double(list: &mut AttrList, value: f64) {
    list.0.push(AttributeValueBytes::Float(value));
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_bool(list: &mut AttrList, value: bool) {
    list.0.push(AttributeValueBytes::Bool(value));
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_bytes(list: &mut AttrList, value: CharSlice) {
    list.0
        .push(AttributeValueBytes::Bytes(Bytes::copy_from_slice(
            value.as_bytes(),
        )));
}

/// Pushes `child`, which is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_list(list: &mut AttrList, child: Box<AttrList>) {
    list.0.push(AttributeValueBytes::List(child.0));
}

/// Pushes `child`, which is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_push_map(list: &mut AttrList, child: Box<AttrMap>) {
    list.0.push(AttributeValueBytes::KeyValue(child.0));
}

// ------------------- Read-back -------------------
//
// Children are fetched by index; returned values borrow the builder and are valid until the next
// push into the same parent, or until it is freed.

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_payload_env(builder: &TracerPayloadBytes) -> CharSlice<'_> {
    char_slice_of(&builder.env)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_payload_app_version(builder: &TracerPayloadBytes) -> CharSlice<'_> {
    char_slice_of(&builder.app_version)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_payload_hostname(builder: &TracerPayloadBytes) -> CharSlice<'_> {
    char_slice_of(&builder.hostname)
}

/// Number of chunks in the builder.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_count(builder: &TracerPayloadBytes) -> usize {
    builder.chunks.len()
}

/// Chunk `idx` (null if out of range).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk(
    builder: &mut TracerPayloadBytes,
    idx: usize,
) -> *mut TraceChunkBytes {
    item_at(&mut builder.chunks, idx)
}

/// Span `idx` of `chunk` (null if out of range), e.g. to re-fetch a span after a sibling push.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span(chunk: &mut TraceChunkBytes, idx: usize) -> *mut SpanBytes {
    item_at(&mut chunk.spans, idx)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link_count(span: &SpanBytes) -> usize {
    span.span_links.len()
}

/// Link `idx` of `span` (null if out of range).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link(span: &mut SpanBytes, idx: usize) -> *mut SpanLinkBytes {
    span.span_links
        .get_mut(idx)
        .map_or(ptr::null_mut(), ptr::from_mut)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_event_count(span: &SpanBytes) -> usize {
    span.span_events.len()
}

/// Event `idx` of `span` (null if out of range).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_event(span: &mut SpanBytes, idx: usize) -> *mut SpanEventBytes {
    span.span_events
        .get_mut(idx)
        .map_or(ptr::null_mut(), ptr::from_mut)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_trace_id_high(chunk: &TraceChunkBytes) -> u64 {
    trace_id_high(&chunk.trace_id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_trace_id_low(chunk: &TraceChunkBytes) -> u64 {
    trace_id_low(&chunk.trace_id)
}

/// Reads the chunk sampling priority; returns `false` (and leaves `out` untouched) when unset.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_sampling_priority(
    chunk: &TraceChunkBytes,
    out: &mut i32,
) -> bool {
    chunk.priority.map(|p| *out = p).is_some()
}

/// Reads the chunk sampling mechanism; returns `false` (and leaves `out` untouched) when unset.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_sampling_mechanism(
    chunk: &TraceChunkBytes,
    out: &mut u32,
) -> bool {
    chunk.sampling_mechanism.map(|m| *out = m).is_some()
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_origin(chunk: &TraceChunkBytes) -> CharSlice<'_> {
    char_slice_of(&chunk.origin)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_service(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.service)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_name(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.name)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_resource(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.resource)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_type(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.r#type)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_env(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.env)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_version(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.version)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_component(span: &SpanBytes) -> CharSlice<'_> {
    char_slice_of(&span.component)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_id(span: &SpanBytes) -> u64 {
    span.span_id
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_parent_id(span: &SpanBytes) -> u64 {
    span.parent_id
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_start(span: &SpanBytes) -> i64 {
    span.start
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_duration(span: &SpanBytes) -> i64 {
    span.duration
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_error(span: &SpanBytes) -> bool {
    span.error
}

/// The span kind as its OTEL wire value.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_kind(span: &SpanBytes) -> u32 {
    span.span_kind as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link_trace_id_high(link: &SpanLinkBytes) -> u64 {
    trace_id_high(&link.trace_id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link_trace_id_low(link: &SpanLinkBytes) -> u64 {
    trace_id_low(&link.trace_id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link_span_id(link: &SpanLinkBytes) -> u64 {
    link.span_id
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_link_tracestate(link: &SpanLinkBytes) -> CharSlice<'_> {
    char_slice_of(&link.tracestate)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_event_time(event: &SpanEventBytes) -> u64 {
    event.time_unix_nano
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_event_name(event: &SpanEventBytes) -> CharSlice<'_> {
    char_slice_of(&event.name)
}

// ---- Attribute maps and values ----

/// One attribute value inside an [`Attributes`] map or a list. Opaque to C.
#[repr(transparent)]
pub struct AttrValue(AttributeValueBytes);

#[inline]
fn value_handle(value: &AttributeValueBytes) -> *const AttrValue {
    ptr::from_ref(value).cast()
}

/// Number of entries in `attrs`, including not-yet-deduped repeated keys (the last one wins).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_attributes_len(attrs: &Attributes) -> usize {
    attrs.0.len()
}

/// Key of the entry at `idx` (empty if out of range).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_attributes_key(attrs: &Attributes, idx: usize) -> CharSlice<'_> {
    match attrs.0.iter().nth(idx) {
        Some((k, _)) => char_slice_of(k),
        None => CharSlice::empty(),
    }
}

/// Value of the entry at `idx` (null if out of range).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_attributes_value(attrs: &Attributes, idx: usize) -> *const AttrValue {
    match attrs.0.iter().nth(idx) {
        Some((_, v)) => value_handle(v),
        None => ptr::null(),
    }
}

/// [`DDOG_V1_ATTR_*`] type tag of `value`.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_type(value: &AttrValue) -> u32 {
    match value.0 {
        AttributeValueBytes::String(_) => DDOG_V1_ATTR_STRING,
        AttributeValueBytes::Int(_) => DDOG_V1_ATTR_INT,
        AttributeValueBytes::Float(_) => DDOG_V1_ATTR_DOUBLE,
        AttributeValueBytes::Bool(_) => DDOG_V1_ATTR_BOOL,
        AttributeValueBytes::Bytes(_) => DDOG_V1_ATTR_BYTES,
        AttributeValueBytes::KeyValue(_) => DDOG_V1_ATTR_KEYVALUE,
        AttributeValueBytes::List(_) => DDOG_V1_ATTR_LIST,
    }
}

/// The string (empty unless a `String`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_str(value: &AttrValue) -> CharSlice<'_> {
    match &value.0 {
        AttributeValueBytes::String(s) => char_slice_of(s),
        _ => CharSlice::empty(),
    }
}

/// The bytes (empty unless `Bytes`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_bytes(value: &AttrValue) -> CharSlice<'_> {
    match &value.0 {
        // Safety: the slice borrows `b`, which lives as long as the value.
        AttributeValueBytes::Bytes(b) => unsafe {
            CharSlice::from_raw_parts(b.as_ref().as_ptr().cast(), b.len())
        },
        _ => CharSlice::empty(),
    }
}

/// The integer (0 unless an `Int`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_int(value: &AttrValue) -> i64 {
    match value.0 {
        AttributeValueBytes::Int(v) => v,
        _ => 0,
    }
}

/// The double (0.0 unless a `Float`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_double(value: &AttrValue) -> f64 {
    match value.0 {
        AttributeValueBytes::Float(v) => v,
        _ => 0.0,
    }
}

/// The boolean (false unless a true `Bool`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_bool(value: &AttrValue) -> bool {
    matches!(value.0, AttributeValueBytes::Bool(true))
}

/// Number of elements (0 unless a `List`).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_list_len(value: &AttrValue) -> usize {
    match &value.0 {
        AttributeValueBytes::List(l) => l.len(),
        _ => 0,
    }
}

/// Element `idx` (null unless a `List` with that element).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_list_get(value: &AttrValue, idx: usize) -> *const AttrValue {
    match &value.0 {
        AttributeValueBytes::List(l) => l.get(idx).map_or(ptr::null(), value_handle),
        _ => ptr::null(),
    }
}

/// The members of a `KeyValue`, read with the `ddog_v1_attributes_*` family (null otherwise).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_value_map(value: &AttrValue) -> *const Attributes {
    match &value.0 {
        AttributeValueBytes::KeyValue(m) => ptr::from_ref(m).cast(),
        _ => ptr::null(),
    }
}

// ------------------- Payload-level metadata -------------------

/// Populates the send-time payload fields (env / app_version / hostname / git come from the
/// builder).
pub fn populate_payload_metadata(
    payload: &mut TracerPayloadBytes,
    container_id: &str,
    language_name: &str,
    language_version: &str,
    tracer_version: &str,
    runtime_id: &str,
) {
    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap_or_default()
    }
    payload.container_id = bs(container_id);
    payload.language_name = bs(language_name);
    payload.language_version = bs(language_version);
    payload.tracer_version = bs(tracer_version);
    payload.runtime_id = bs(runtime_id);
}

// ------------------- Debug logging -------------------

/// Renders 16 big-endian trace-id bytes as a 32-char lowercase hex string.
fn hex16(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Renders a typed V1 attribute value in a compact, readable form (strings quoted, byte blobs shown
/// as a length, key-values/lists rendered recursively).
fn render_attr_value(value: &AttributeValueBytes) -> String {
    match value {
        AttributeValueBytes::String(s) => format!("{:?}", s.as_str()),
        AttributeValueBytes::Int(i) => i.to_string(),
        AttributeValueBytes::Float(f) => f.to_string(),
        AttributeValueBytes::Bool(b) => b.to_string(),
        AttributeValueBytes::Bytes(b) => format!("<{} bytes>", b.len()),
        AttributeValueBytes::KeyValue(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{}: {}", k.as_str(), render_attr_value(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        AttributeValueBytes::List(list) => {
            let inner: Vec<String> = list.iter().map(render_attr_value).collect();
            format!("[{}]", inner.join(", "))
        }
    }
}

/// Renders a V1 span (plus its chunk's trace id) as a readable diagnostic string.
fn render_span_debug(span: &SpanBytes, chunk: &TraceChunkBytes) -> String {
    let mut out = String::new();
    let _ = write!(out, "trace_id={} ", hex16(&chunk.trace_id));
    let _ = write!(
        out,
        "service={:?} name={:?} resource={:?} type={:?} span_id={} parent_id={} \
         start={} duration={} error={} kind={:?} env={:?} version={:?} component={:?}",
        span.service.as_str(),
        span.name.as_str(),
        span.resource.as_str(),
        span.r#type.as_str(),
        span.span_id,
        span.parent_id,
        span.start,
        span.duration,
        span.error,
        span.span_kind,
        span.env.as_str(),
        span.version.as_str(),
        span.component.as_str(),
    );
    let attrs: Vec<String> = span
        .attributes
        .iter()
        .map(|(k, v)| format!("{}={}", k.as_str(), render_attr_value(v)))
        .collect();
    let _ = write!(
        out,
        " attributes={{{}}} links={} events={}",
        attrs.join(", "),
        span.span_links.len(),
        span.span_events.len(),
    );
    out
}

/// Renders a span for a tracer's debug log. The returned owned slice must be freed with
/// [`ddog_free_charslice`].
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_span_debug_log(
    chunk: &TraceChunkBytes,
    span: &SpanBytes,
) -> CharSlice<'static> {
    let debug_str = render_span_debug(span, chunk);
    // An empty (or NUL-containing, hence unrepresentable) render owns no allocation: return a
    // borrowed empty slice so a zero length always means "nothing to free" in
    // `ddog_free_charslice`.
    let cstring = match CString::new(debug_str) {
        Ok(c) if !c.as_bytes().is_empty() => c,
        _ => return CharSlice::empty(),
    };
    let len = cstring.as_bytes().len();

    // Safety: `CString` owns a `len + 1` byte allocation (payload + NUL); the pointer is freed by
    // `ddog_free_charslice`, which reclaims that same shape.
    unsafe { CharSlice::from_raw_parts(cstring.into_raw().cast(), len) }
}

// ------------------- Shared free helper -------------------

/// Frees an owned [`CharSlice`]. Only the few functions that document it return owned slices (the
/// V1 [`ddog_v1_span_debug_log`] and the v0.4
/// [`crate::span_v04::ddog_serialize_trace_into_charslice`]); borrowed slices must NOT be passed
/// here. An owned slice allocates `len + 1` bytes (payload + NUL), reclaimed here with that exact
/// shape; a zero-length slice is always borrowed and owns nothing.
///
/// # Safety
///
/// `slice` must be an owned char slice that has been returned by one of the functions of this API.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_free_charslice(slice: CharSlice<'static>) {
    let (ptr, len) = slice.as_raw_parts();

    if len == 0 || ptr.is_null() {
        return;
    }

    // The allocation is `len + 1` bytes (payload + trailing NUL); reconstruct the same layout.
    unsafe {
        let _ = Vec::from_raw_parts(ptr as *mut u8, len + 1, len + 1);
    }
}
