// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Box-per-node FFI builder for the native V1 trace payload
//! ([`libdd_trace_utils::span::v1::TracerPayload`]), storing readable [`BytesString`]s directly.
//!
//! Each chunk/span/link/event is its OWN heap allocation (`Box::into_raw`), stored as a raw pointer
//! in its parent node. C is handed that node pointer directly and per-node mutators and getters
//! materialize `&mut *ptr` / `&*ptr` against the node's own allocation — so a held node pointer
//! stays valid across sibling pushes (no parent-`Vec` reallocation can move an existing node) and
//! no mutation reborrows `&mut builder` (which would pop the tag of an outstanding node pointer).
//! This is Stacked- and Tree-Borrows clean. The boxes are folded back into the inline payload model
//! by [`TracerPayloadV1Builder::into_payload`], or freed by its [`Drop`]. Payload env / app_version
//! / hostname are set on the builder; the rest is applied at send time (see
//! [`populate_payload_metadata`]).

use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_utils::msgpack_encoder::v04::local_root_idx;
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

/// A chunk node in the builder: its own heap allocation, so a `*mut ChunkNode` handed to C stays
/// valid across sibling chunk pushes. Its spans live as separate `Box` allocations.
pub struct ChunkNode {
    chunk: TraceChunkBytes,
    spans: Vec<*mut SpanNode>,
}

impl ChunkNode {
    pub fn chunk(&self) -> &TraceChunkBytes {
        &self.chunk
    }

    pub fn chunk_mut(&mut self) -> &mut TraceChunkBytes {
        &mut self.chunk
    }

    pub fn spans(&self) -> &[*mut SpanNode] {
        &self.spans
    }

    /// Appends an empty span, returning a pointer to its own (heap) node.
    pub fn push_span(&mut self) -> *mut SpanNode {
        let node = Box::into_raw(Box::new(SpanNode {
            span: SpanBytes::default(),
            links: Vec::new(),
            events: Vec::new(),
        }));
        self.spans.push(node);
        node
    }
}

/// A span node in the builder: its own heap allocation, so a held `*mut SpanNode` stays valid
/// across sibling span pushes into the same chunk (the inferred-span case). Links/events are
/// likewise separate `Box` allocations.
pub struct SpanNode {
    span: SpanBytes,
    links: Vec<*mut SpanLinkBytes>,
    events: Vec<*mut SpanEventBytes>,
}

impl SpanNode {
    pub fn span(&self) -> &SpanBytes {
        &self.span
    }

    pub fn span_mut(&mut self) -> &mut SpanBytes {
        &mut self.span
    }

    pub fn links(&self) -> &[*mut SpanLinkBytes] {
        &self.links
    }

    pub fn events(&self) -> &[*mut SpanEventBytes] {
        &self.events
    }

    /// Appends an empty link, returning a pointer to its own (heap) allocation.
    pub fn push_link(&mut self) -> *mut SpanLinkBytes {
        let link = Box::into_raw(Box::new(SpanLinkBytes::default()));
        self.links.push(link);
        link
    }

    /// Appends an empty event, returning a pointer to its own (heap) allocation.
    pub fn push_event(&mut self) -> *mut SpanEventBytes {
        let event = Box::into_raw(Box::new(SpanEventBytes::default()));
        self.events.push(event);
        event
    }
}

/// Frees a chunk node and everything below it.
///
/// # Safety
/// `ptr` must be a live `Box::into_raw(ChunkNode)` allocation not freed elsewhere.
unsafe fn free_chunk_node(ptr: *mut ChunkNode) {
    // Safety: per the fn contract; each span pointer is likewise a live, unshared allocation.
    let node = unsafe { Box::from_raw(ptr) };
    for &span in &node.spans {
        unsafe { free_span_node(span) };
    }
}

/// Frees a span node and its links/events.
///
/// # Safety
/// `ptr` must be a live `Box::into_raw(SpanNode)` allocation not freed elsewhere.
unsafe fn free_span_node(ptr: *mut SpanNode) {
    // Safety: per the fn contract; link/event pointers are live, unshared allocations.
    let node = unsafe { Box::from_raw(ptr) };
    for &link in &node.links {
        drop(unsafe { Box::from_raw(link) });
    }
    for &event in &node.events {
        drop(unsafe { Box::from_raw(event) });
    }
}

/// Builds a native V1 [`TracerPayloadBytes`] holding readable strings. Each node is its own heap
/// allocation (see the module docs); the builder owns the top-level chunk pointers.
#[derive(Default)]
pub struct TracerPayloadV1Builder {
    chunks: Vec<*mut ChunkNode>,
    attributes: AttrVecMap,
    env: BytesString,
    app_version: BytesString,
    hostname: BytesString,
}

// SAFETY: the builder owns its node boxes and is only ever driven from a single tracer thread,
// then consumed synchronously by the send path; the raw pointers carry no cross-thread state.
unsafe impl Send for TracerPayloadV1Builder {}

impl TracerPayloadV1Builder {
    pub fn chunks(&self) -> &[*mut ChunkNode] {
        &self.chunks
    }

    /// Payload-level attributes (the TracerPayload `attributes` map).
    pub fn attributes(&self) -> &AttrVecMap {
        &self.attributes
    }

    /// Sets the payload `env` / `app_version` / `hostname` fields.
    pub fn set_metadata(
        &mut self,
        env: BytesString,
        app_version: BytesString,
        hostname: BytesString,
    ) {
        self.env = env;
        self.app_version = app_version;
        self.hostname = hostname;
    }

    /// Appends an empty chunk with the given 128-bit trace id (high/low halves), returning a
    /// pointer to its own (heap) node.
    pub fn push_chunk(&mut self, trace_id_high: u64, trace_id_low: u64) -> *mut ChunkNode {
        let node = Box::into_raw(Box::new(ChunkNode {
            chunk: TraceChunkBytes {
                trace_id: trace_id_bytes(trace_id_high, trace_id_low),
                ..Default::default()
            },
            spans: Vec::new(),
        }));
        self.chunks.push(node);
        node
    }

    /// Consumes the builder, folding the node boxes back into the inline payload model. The one
    /// dedup of the send path runs here, where the maps were filled (possibly overwriting keys).
    pub fn into_payload(mut self) -> TracerPayloadBytes {
        // Move the chunk pointers out so `Drop` (which runs at the end of this fn over the
        // now-empty `chunks`) never double-frees the nodes reclaimed below.
        let chunks = std::mem::take(&mut self.chunks);
        let mut payload = TracerPayloadBytes {
            attributes: std::mem::take(&mut self.attributes),
            env: std::mem::take(&mut self.env),
            app_version: std::mem::take(&mut self.app_version),
            hostname: std::mem::take(&mut self.hostname),
            ..Default::default()
        };
        for cptr in chunks {
            // Safety: `cptr` is a live `Box::into_raw` allocation, moved out of `self` and
            // reclaimed exactly once here.
            let ChunkNode { mut chunk, spans } = *unsafe { Box::from_raw(cptr) };
            for sptr in spans {
                // Safety: as above.
                let SpanNode {
                    mut span,
                    links,
                    events,
                } = *unsafe { Box::from_raw(sptr) };
                span.span_links = links
                    .into_iter()
                    .map(|l| *unsafe { Box::from_raw(l) })
                    .collect();
                span.span_events = events
                    .into_iter()
                    .map(|e| *unsafe { Box::from_raw(e) })
                    .collect();
                chunk.spans.push(span);
            }
            payload.chunks.push(chunk);
        }
        payload.dedup();
        payload
    }
}

impl Drop for TracerPayloadV1Builder {
    fn drop(&mut self) {
        for &cptr in &self.chunks {
            // Safety: every pointer in `chunks` is a live `Box::into_raw` allocation;
            // `into_payload` empties `chunks` before drop, so a node is never freed
            // twice.
            unsafe { free_chunk_node(cptr) };
        }
    }
}

/// Composes a 128-bit trace id from its high/low 64-bit halves into 16 big-endian bytes.
pub fn trace_id_bytes(high: u64, low: u64) -> [u8; 16] {
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
pub fn bytes_string_from_slice(slice: CharSlice) -> BytesString {
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
pub unsafe fn bytes_string_from_literal(string: *const c_char) -> BytesString {
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
pub extern "C" fn ddog_v1_new_builder() -> Box<TracerPayloadV1Builder> {
    Box::default()
}

/// Frees a V1 payload builder.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_free_builder(_builder: Box<TracerPayloadV1Builder>) {}

/// Sets the payload `env` / `app_version` / `hostname` fields (empty = unset).
#[unsafe(no_mangle)]
pub extern "C" fn ddog_set_payload_metadata(
    builder: &mut TracerPayloadV1Builder,
    env: CharSlice,
    app_version: CharSlice,
    hostname: CharSlice,
) {
    builder.set_metadata(
        bytes_string_from_slice(env),
        bytes_string_from_slice(app_version),
        bytes_string_from_slice(hostname),
    );
}

// ------------------- Chunk / span / link / event creation -------------------

/// Appends a chunk carrying the 128-bit trace id (high/low halves), returning its node pointer.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_new_chunk(
    builder: &mut TracerPayloadV1Builder,
    trace_id_high: u64,
    trace_id_low: u64,
) -> *mut ChunkNode {
    builder.push_chunk(trace_id_high, trace_id_low)
}

/// Number of spans already in `chunk`.
///
/// # Safety
/// `chunk` must be a live chunk node pointer from [`ddog_new_chunk`] (applies to every chunk fn).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_chunk_span_count(chunk: *mut ChunkNode) -> usize {
    unsafe { (*chunk).spans.len() }
}

/// Appends an empty span to `chunk`, returning its node pointer.
///
/// # Safety
/// See [`ddog_chunk_span_count`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_new_span(chunk: *mut ChunkNode) -> *mut SpanNode {
    unsafe { (*chunk).push_span() }
}

/// Appends an empty link to `span`, returning its node pointer.
///
/// # Safety
/// `span` must be a live span node pointer from [`ddog_new_span`] (applies to every span fn).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_new_link(span: *mut SpanNode) -> *mut SpanLinkBytes {
    unsafe { (*span).push_link() }
}

/// Appends an empty event to `span`, returning its node pointer.
///
/// # Safety
/// See [`ddog_new_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_new_event(span: *mut SpanNode) -> *mut SpanEventBytes {
    unsafe { (*span).push_event() }
}

// ------------------- Span fields -------------------

/// # Safety
/// See [`ddog_new_link`] (applies to every `ddog_span_set_*` / `ddog_set_span_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_set_id(span: *mut SpanNode, value: u64) {
    unsafe { (*span).span.span_id = value };
}

/// # Safety
/// See [`ddog_span_set_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_set_parent_id(span: *mut SpanNode, value: u64) {
    unsafe { (*span).span.parent_id = value };
}

/// # Safety
/// See [`ddog_span_set_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_set_start(span: *mut SpanNode, value: i64) {
    unsafe { (*span).span.start = value };
}

/// # Safety
/// See [`ddog_span_set_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_set_duration(span: *mut SpanNode, value: i64) {
    unsafe { (*span).span.duration = value };
}

/// # Safety
/// See [`ddog_span_set_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_set_error(span: *mut SpanNode, error: bool) {
    unsafe { (*span).span.error = error };
}

/// Reads the span error flag (e.g. to mirror it onto an inferred span).
///
/// # Safety
/// See [`ddog_new_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_get_error(span: *mut SpanNode) -> bool {
    unsafe { (*span).span.error }
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_service(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.service, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_name(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.name, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_resource(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.resource, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_type(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.r#type, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_env(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.env, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_version(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.version, value) };
}

/// # Safety
/// See [`ddog_span_get_error`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_component(span: *mut SpanNode, value: CharSlice) {
    unsafe { set_field(&mut (*span).span.component, value) };
}

/// Sets the span kind from an OTEL wire value (unset/unknown → Unspecified).
///
/// # Safety
/// See [`ddog_new_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_kind(span: *mut SpanNode, kind: u32) {
    unsafe { (*span).span.span_kind = SpanKind::from(kind) };
}

/// Sets the span kind from a v0.4 `span.kind` string. Returns `false` for a non-canonical kind,
/// which has no wire slot, so the caller keeps it as a plain attribute.
///
/// # Safety
/// See [`ddog_new_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_span_kind_str(span: *mut SpanNode, value: CharSlice) -> bool {
    let kind = SpanKind::from_meta(&String::from_utf8_lossy(value.as_bytes()));
    unsafe { (*span).span.span_kind = kind };
    kind != SpanKind::Unspecified
}

// ------------------- Chunk fields -------------------

/// # Safety
/// See [`ddog_chunk_span_count`] (applies to every `ddog_set_chunk_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_chunk_origin(chunk: *mut ChunkNode, origin: CharSlice) {
    unsafe { set_field(&mut (*chunk).chunk.origin, origin) };
}

/// # Safety
/// See [`ddog_set_chunk_origin`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_chunk_sampling_priority(chunk: *mut ChunkNode, priority: i32) {
    unsafe { (*chunk).chunk.priority = Some(priority) };
}

/// # Safety
/// See [`ddog_set_chunk_origin`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_chunk_sampling_mechanism(chunk: *mut ChunkNode, mechanism: u32) {
    unsafe { (*chunk).chunk.sampling_mechanism = Some(mechanism) };
}

// ------------------- Link / event fields -------------------

/// # Safety
/// `link` must be a live link node pointer from [`ddog_new_link`] (applies to every
/// `ddog_link_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_set_trace_id(
    link: *mut SpanLinkBytes,
    trace_id_high: u64,
    trace_id_low: u64,
) {
    unsafe { (*link).trace_id = trace_id_bytes(trace_id_high, trace_id_low) };
}

/// # Safety
/// See [`ddog_link_set_trace_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_set_span_id(link: *mut SpanLinkBytes, value: u64) {
    unsafe { (*link).span_id = value };
}

/// # Safety
/// See [`ddog_link_set_trace_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_set_flags(link: *mut SpanLinkBytes, value: u32) {
    unsafe { (*link).flags = value };
}

/// # Safety
/// See [`ddog_link_set_trace_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_set_tracestate(link: *mut SpanLinkBytes, value: CharSlice) {
    unsafe { set_field(&mut (*link).tracestate, value) };
}

/// # Safety
/// `event` must be a live event node pointer from [`ddog_new_event`] (applies to every
/// `ddog_event_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_event_set_name(event: *mut SpanEventBytes, value: CharSlice) {
    unsafe { set_field(&mut (*event).name, value) };
}

/// # Safety
/// See [`ddog_event_set_name`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_event_set_time(event: *mut SpanEventBytes, time_unix_nano: u64) {
    unsafe { (*event).time_unix_nano = time_unix_nano };
}

// ------------------- Attributes -------------------
//
// Every attribute map is filled through the `ddog_attributes_add_*` family and read through the
// `ddog_v1_attributes_*` family, on an `Attributes` handle. A node's handle is a raw place
// projection off the node pointer (no intermediate `&mut`), so it shares the node pointer's tag
// and stays valid across other calls on the same node (Stacked and Tree Borrows).

/// Opaque handle to an attribute map: the payload's, a chunk's, span's, link's or event's
/// (`ddog_*_get_attributes`), or an owned nested map's ([`ddog_attr_map_get_attributes`]).
pub struct Attributes {
    _private: [u8; 0],
}

#[inline]
fn attributes_handle(map: *mut AttrVecMap) -> *mut Attributes {
    map.cast()
}

/// The map behind an [`Attributes`] handle.
///
/// # Safety
/// `attrs` must be a live handle, not aliased by another live reference for `'a`.
#[inline]
pub unsafe fn attrs_mut<'a>(attrs: *mut Attributes) -> &'a mut AttrVecMap {
    unsafe { &mut *attrs.cast::<AttrVecMap>() }
}

/// # Safety
/// `attrs` must be a live handle.
#[inline]
unsafe fn attrs_ref<'a>(attrs: *const Attributes) -> &'a AttrVecMap {
    unsafe { &*attrs.cast::<AttrVecMap>() }
}

/// The payload's attribute map.
///
/// # Safety
/// `builder` must be a live builder from [`ddog_v1_new_builder`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_payload_get_attributes(
    builder: *mut TracerPayloadV1Builder,
) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*builder).attributes })
}

/// # Safety
/// See [`ddog_chunk_span_count`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_chunk_get_attributes(chunk: *mut ChunkNode) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*chunk).chunk.attributes })
}

/// # Safety
/// See [`ddog_new_link`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_span_get_attributes(span: *mut SpanNode) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*span).span.attributes })
}

/// # Safety
/// See [`ddog_link_set_trace_id`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_link_get_attributes(link: *mut SpanLinkBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*link).attributes })
}

/// # Safety
/// See [`ddog_event_set_name`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_event_get_attributes(event: *mut SpanEventBytes) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*event).attributes })
}

/// # Safety
/// `attrs` must be a live [`Attributes`] handle (applies to every `ddog_attributes_add_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_str(
    attrs: *mut Attributes,
    key: CharSlice,
    value: CharSlice,
) {
    let value = AttributeValueBytes::String(bytes_string_from_slice(value));
    unsafe { attrs_mut(attrs) }.insert(bytes_string_from_slice(key), value);
}

/// # Safety
/// See [`ddog_attributes_add_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_int(
    attrs: *mut Attributes,
    key: CharSlice,
    value: i64,
) {
    unsafe { attrs_mut(attrs) }.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Int(value),
    );
}

/// # Safety
/// See [`ddog_attributes_add_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_double(
    attrs: *mut Attributes,
    key: CharSlice,
    value: f64,
) {
    unsafe { attrs_mut(attrs) }.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Float(value),
    );
}

/// # Safety
/// See [`ddog_attributes_add_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_bool(
    attrs: *mut Attributes,
    key: CharSlice,
    value: bool,
) {
    unsafe { attrs_mut(attrs) }.insert(
        bytes_string_from_slice(key),
        AttributeValueBytes::Bool(value),
    );
}

/// Bytes attribute (v0.4 `meta_struct`), copied verbatim.
///
/// # Safety
/// See [`ddog_attributes_add_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_bytes(
    attrs: *mut Attributes,
    key: CharSlice,
    value: CharSlice,
) {
    let value = AttributeValueBytes::Bytes(Bytes::copy_from_slice(value.as_bytes()));
    unsafe { attrs_mut(attrs) }.insert(bytes_string_from_slice(key), value);
}

/// Sets `attrs[key]` to `list`, which is consumed.
///
/// # Safety
/// See [`ddog_attributes_add_str`]; `list` must be a live list from [`ddog_attr_list_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_list(
    attrs: *mut Attributes,
    key: CharSlice,
    list: *mut AttrList,
) {
    let value = unsafe { take_list(list) };
    unsafe { attrs_mut(attrs) }.insert(bytes_string_from_slice(key), value);
}

/// Sets `attrs[key]` to `map`, which is consumed.
///
/// # Safety
/// See [`ddog_attributes_add_str`]; `map` must be a live map from [`ddog_attr_map_new`], other than
/// the one `attrs` belongs to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attributes_add_map(
    attrs: *mut Attributes,
    key: CharSlice,
    map: *mut AttrMap,
) {
    let value = unsafe { take_map(map) };
    unsafe { attrs_mut(attrs) }.insert(bytes_string_from_slice(key), value);
}

/// Copies the attribute `key` from `from_span` onto `to_span`, returning whether the source had it;
/// removes it from the source when `delete_source` is set. Type-preserving.
///
/// # Safety
/// `from_span`/`to_span` must be live, distinct span node pointers from [`ddog_new_span`]; `key`
/// a static NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_transfer_span_attr(
    from_span: *mut SpanNode,
    to_span: *mut SpanNode,
    key: *const c_char,
    delete_source: bool,
) -> bool {
    let key = unsafe { bytes_string_from_literal(key) };
    // The two spans are distinct allocations, so the read-clone and the writes don't alias.
    let value = match unsafe { (*from_span).span.attributes.get(&key) } {
        Some(v) => clone_attr(v),
        None => return false,
    };
    unsafe { (*to_span).span.attributes.insert(key.clone(), value) };
    if delete_source {
        unsafe { (*from_span).span.attributes.remove_slow(&key) };
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

#[inline]
unsafe fn take_list(list: *mut AttrList) -> AttributeValueBytes {
    AttributeValueBytes::List(unsafe { Box::from_raw(list) }.0)
}

#[inline]
unsafe fn take_map(map: *mut AttrMap) -> AttributeValueBytes {
    AttributeValueBytes::KeyValue(unsafe { Box::from_raw(map) }.0)
}

/// Allocates an empty list with room for `capacity` elements, owned by C until it is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_list_new(capacity: usize) -> *mut AttrList {
    Box::into_raw(Box::new(AttrList(Vec::with_capacity(capacity))))
}

/// Allocates an empty map with room for `capacity` members, owned by C until it is consumed.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_attr_map_new(capacity: usize) -> *mut AttrMap {
    Box::into_raw(Box::new(AttrMap(VecMap::with_capacity(capacity))))
}

/// The attribute map of an owned nested `map`, filled with the `ddog_attributes_add_*` family.
///
/// # Safety
/// `map` must be a live map from [`ddog_attr_map_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_map_get_attributes(map: *mut AttrMap) -> *mut Attributes {
    attributes_handle(unsafe { &raw mut (*map).0 })
}

/// # Safety
/// `list` must be a live list from [`ddog_attr_list_new`] (applies to every `ddog_attr_list_*`);
/// a `child` is consumed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_str(list: *mut AttrList, value: CharSlice) {
    unsafe {
        (*list)
            .0
            .push(AttributeValueBytes::String(bytes_string_from_slice(value)))
    };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_int(list: *mut AttrList, value: i64) {
    unsafe { (*list).0.push(AttributeValueBytes::Int(value)) };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_double(list: *mut AttrList, value: f64) {
    unsafe { (*list).0.push(AttributeValueBytes::Float(value)) };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_bool(list: *mut AttrList, value: bool) {
    unsafe { (*list).0.push(AttributeValueBytes::Bool(value)) };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_bytes(list: *mut AttrList, value: CharSlice) {
    let value = AttributeValueBytes::Bytes(Bytes::copy_from_slice(value.as_bytes()));
    unsafe { (*list).0.push(value) };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_list(list: *mut AttrList, child: *mut AttrList) {
    unsafe { (*list).0.push(take_list(child)) };
}

/// # Safety
/// See [`ddog_attr_list_push_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_attr_list_push_map(list: *mut AttrList, child: *mut AttrMap) {
    unsafe { (*list).0.push(take_map(child)) };
}

// ------------------- Read-back -------------------
//
// Getters take the node pointers directly; children come as the parent's node-pointer array, so a
// reader walks the tree without re-resolving any index. Returned slices borrow the builder and are
// valid until it is next mutated or freed.

/// The builder's chunk node pointers; `len` receives their count.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunks(
    builder: &TracerPayloadV1Builder,
    len: &mut usize,
) -> *const *mut ChunkNode {
    *len = builder.chunks.len();
    builder.chunks.as_ptr()
}

/// Number of chunks in the builder.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_count(builder: &TracerPayloadV1Builder) -> usize {
    builder.chunks.len()
}

/// The chunk's span node pointers; `len` receives their count.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_spans(chunk: &ChunkNode, len: &mut usize) -> *const *mut SpanNode {
    *len = chunk.spans.len();
    chunk.spans.as_ptr()
}

/// The chunk's local-root span, as the v0.4 wire picks it (`local_root_idx`), or null if empty.
/// Chunk-level trace tags (trace_id_high, sampling priority/mechanism, origin) belong on it only.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_root_span(chunk: &ChunkNode) -> *mut SpanNode {
    // Safety: the span pointers are live allocations owned by the chunk.
    let root = local_root_idx(chunk.spans.iter().map(|&s| unsafe { &(*s).span }));
    chunk.spans.get(root).copied().unwrap_or(ptr::null_mut())
}

/// The span's link node pointers; `len` receives their count.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_links(span: &SpanNode, len: &mut usize) -> *const *mut SpanLinkBytes {
    *len = span.links.len();
    span.links.as_ptr()
}

/// The span's event node pointers; `len` receives their count.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_events(
    span: &SpanNode,
    len: &mut usize,
) -> *const *mut SpanEventBytes {
    *len = span.events.len();
    span.events.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_trace_id_high(chunk: &ChunkNode) -> u64 {
    trace_id_high(&chunk.chunk.trace_id)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_trace_id_low(chunk: &ChunkNode) -> u64 {
    trace_id_low(&chunk.chunk.trace_id)
}

/// Reads the chunk sampling priority; returns `false` (and leaves `out` untouched) when unset.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_sampling_priority(chunk: &ChunkNode, out: &mut i32) -> bool {
    chunk.chunk.priority.map(|p| *out = p).is_some()
}

/// Reads the chunk sampling mechanism; returns `false` (and leaves `out` untouched) when unset.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_sampling_mechanism(chunk: &ChunkNode, out: &mut u32) -> bool {
    chunk.chunk.sampling_mechanism.map(|m| *out = m).is_some()
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_chunk_origin(chunk: &ChunkNode) -> CharSlice<'_> {
    char_slice_of(&chunk.chunk.origin)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_service(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.service)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_name(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.name)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_resource(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.resource)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_type(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.r#type)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_env(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.env)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_version(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.version)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_component(span: &SpanNode) -> CharSlice<'_> {
    char_slice_of(&span.span.component)
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_id(span: &SpanNode) -> u64 {
    span.span.span_id
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_parent_id(span: &SpanNode) -> u64 {
    span.span.parent_id
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_start(span: &SpanNode) -> i64 {
    span.span.start
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_duration(span: &SpanNode) -> i64 {
    span.span.duration
}

#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_error(span: &SpanNode) -> bool {
    span.span.error
}

/// The span kind as its OTEL wire value.
#[unsafe(no_mangle)]
pub extern "C" fn ddog_v1_get_span_kind(span: &SpanNode) -> u32 {
    span.span.span_kind as u32
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
pub extern "C" fn ddog_v1_get_link_flags(link: &SpanLinkBytes) -> u32 {
    link.flags
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

/// Opaque handle to one attribute value inside an [`Attributes`] map or a list.
pub struct AttrValue {
    _private: [u8; 0],
}

#[inline]
fn value_handle(value: &AttributeValueBytes) -> *const AttrValue {
    (value as *const AttributeValueBytes).cast()
}

/// # Safety
/// `value` must be a live handle from a `ddog_v1_*` getter.
#[inline]
unsafe fn value_ref<'a>(value: *const AttrValue) -> &'a AttributeValueBytes {
    unsafe { &*value.cast::<AttributeValueBytes>() }
}

/// Number of entries in `attrs`, including not-yet-deduped repeated keys (the last one wins).
///
/// # Safety
/// `attrs` must be a live [`Attributes`] handle (applies to every `ddog_v1_attributes_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_attributes_len(attrs: *const Attributes) -> usize {
    unsafe { attrs_ref(attrs) }.len()
}

/// Key of the entry at `idx` (empty if out of range).
///
/// # Safety
/// See [`ddog_v1_attributes_len`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_attributes_key<'a>(
    attrs: *const Attributes,
    idx: usize,
) -> CharSlice<'a> {
    match unsafe { attrs_ref(attrs) }.iter().nth(idx) {
        Some((k, _)) => char_slice_of(k),
        None => CharSlice::empty(),
    }
}

/// Value of the entry at `idx` (null if out of range).
///
/// # Safety
/// See [`ddog_v1_attributes_len`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_attributes_value(
    attrs: *const Attributes,
    idx: usize,
) -> *const AttrValue {
    match unsafe { attrs_ref(attrs) }.iter().nth(idx) {
        Some((_, v)) => value_handle(v),
        None => ptr::null(),
    }
}

/// Value of `key` (null if absent).
///
/// # Safety
/// See [`ddog_v1_attributes_len`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_attributes_get(
    attrs: *const Attributes,
    key: CharSlice,
) -> *const AttrValue {
    let key = String::from_utf8_lossy(key.as_bytes());
    match unsafe { attrs_ref(attrs) }.get(key.as_ref()) {
        Some(v) => value_handle(v),
        None => ptr::null(),
    }
}

/// [`DDOG_V1_ATTR_*`] type tag of `value`.
///
/// # Safety
/// `value` must be a live [`AttrValue`] handle (applies to every `ddog_v1_value_*`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_type(value: *const AttrValue) -> u32 {
    match unsafe { value_ref(value) } {
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
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_str<'a>(value: *const AttrValue) -> CharSlice<'a> {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::String(s) => char_slice_of(s),
        _ => CharSlice::empty(),
    }
}

/// The bytes (empty unless `Bytes`).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_bytes<'a>(value: *const AttrValue) -> CharSlice<'a> {
    match unsafe { value_ref(value) } {
        // Safety: the slice borrows `b`, which lives as long as the value.
        AttributeValueBytes::Bytes(b) => unsafe {
            CharSlice::from_raw_parts(b.as_ref().as_ptr().cast(), b.len())
        },
        _ => CharSlice::empty(),
    }
}

/// The integer (0 unless an `Int`).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_int(value: *const AttrValue) -> i64 {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::Int(v) => *v,
        _ => 0,
    }
}

/// The double (0.0 unless a `Float`).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_double(value: *const AttrValue) -> f64 {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::Float(v) => *v,
        _ => 0.0,
    }
}

/// The boolean (false unless a true `Bool`).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_bool(value: *const AttrValue) -> bool {
    matches!(unsafe { value_ref(value) }, AttributeValueBytes::Bool(true))
}

/// Number of elements (0 unless a `List`).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_list_len(value: *const AttrValue) -> usize {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::List(l) => l.len(),
        _ => 0,
    }
}

/// Element `idx` (null unless a `List` with that element).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_list_get(
    value: *const AttrValue,
    idx: usize,
) -> *const AttrValue {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::List(l) => l.get(idx).map_or(ptr::null(), value_handle),
        _ => ptr::null(),
    }
}

/// The members of a `KeyValue`, read with the `ddog_v1_attributes_*` family (null otherwise).
///
/// # Safety
/// See [`ddog_v1_value_type`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_value_map(value: *const AttrValue) -> *const Attributes {
    match unsafe { value_ref(value) } {
        AttributeValueBytes::KeyValue(m) => (m as *const AttrVecMap).cast(),
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

/// Renders a V1 span (plus its chunk's trace id) as a readable diagnostic string. Link/event counts
/// are passed explicitly because, mid-build, they live on the [`SpanNode`], not the span's own
/// (still-empty) inline vectors.
fn render_span_debug(
    span: &SpanBytes,
    chunk: Option<&TraceChunkBytes>,
    links: usize,
    events: usize,
) -> String {
    let mut out = String::new();
    if let Some(c) = chunk {
        let _ = write!(out, "trace_id={} ", hex16(&c.trace_id));
    }
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
        links,
        events,
    );
    out
}

/// Renders a span for dd-trace-php's `DD_TRACE_DEBUG` "Encoding span" line. Takes the owning
/// chunk/span node pointers (the outer frame's still-live handles), so it works mid-build before
/// the nodes are folded into the inline payload. The returned owned slice must be freed with
/// [`ddog_free_charslice`].
///
/// # Safety
/// `chunk`/`span` must be live node pointers previously returned by
/// `ddog_new_chunk`/`ddog_new_span` (with `span` a span of `chunk`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_v1_span_debug_log(
    chunk: *mut ChunkNode,
    span: *mut SpanNode,
) -> CharSlice<'static> {
    // Safety: per the fn contract, both are live node pointers.
    let (chunk_node, span_node) = unsafe { (&*chunk, &*span) };
    let debug_str = render_span_debug(
        span_node.span(),
        Some(&chunk_node.chunk),
        span_node.links.len(),
        span_node.events.len(),
    );
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
