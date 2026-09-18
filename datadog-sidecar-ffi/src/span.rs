// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Box-per-node FFI builder for the native V1 trace payload
//! ([`libdd_trace_utils::span::v1::TracerPayload`]), storing readable [`BytesString`]s directly.
//!
//! Each chunk/span/link/event is its OWN heap allocation (`Box::into_raw`), stored as a raw pointer
//! in its parent node. C is handed that node pointer directly and per-node mutators materialize
//! `&mut *ptr` against the node's own allocation — so a held node pointer stays valid across sibling
//! pushes (no parent-`Vec` reallocation can move an existing node) and no mutation reborrows
//! `&mut builder` (which would pop the tag of an outstanding node pointer). This is Stacked- and
//! Tree-Borrows clean. The boxes are folded back into the inline payload model by
//! [`TracerPayloadV1Builder::into_payload`], or freed by its [`Drop`]. Payload-level metadata is
//! applied at send time (see [`populate_payload_metadata`]).

use libdd_common_ffi::slice::CharSlice;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v1::{
    AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes, TraceChunkBytes,
    TracerPayloadBytes,
};
use libdd_trace_utils::span::vec_map::VecMap;
use std::ffi::CString;
use std::fmt::Write as _;

/// Attribute value type tags from `ddog_v1_get_*_attr_type`, so a C caller picks the matching typed
/// value getter (`_attr_str`/`_attr_int`/`_attr_double`/`_attr_bool`/`_attr_bytes`).
pub const DDOG_V1_ATTR_STRING: u32 = 0;
pub const DDOG_V1_ATTR_INT: u32 = 1;
pub const DDOG_V1_ATTR_DOUBLE: u32 = 2;
pub const DDOG_V1_ATTR_BOOL: u32 = 3;
pub const DDOG_V1_ATTR_BYTES: u32 = 4;
pub const DDOG_V1_ATTR_KEYVALUE: u32 = 5;
pub const DDOG_V1_ATTR_LIST: u32 = 6;

/// A chunk node in the builder: its own heap allocation, so a `*mut ChunkNode` handed to C stays
/// valid across sibling chunk pushes. Holds the chunk's scalar fields/attributes inline; its spans
/// live as separate `Box` allocations (raw pointers here), folded into `chunk.spans` by
/// [`TracerPayloadV1Builder::into_payload`].
pub struct ChunkNode {
    chunk: TraceChunkBytes,
    spans: Vec<*mut SpanNode>,
}

impl ChunkNode {
    pub fn chunk_mut(&mut self) -> &mut TraceChunkBytes {
        &mut self.chunk
    }

    pub fn span_count(&self) -> usize {
        self.spans.len()
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

/// A span node in the builder: its own heap allocation, so a held `*mut SpanNode` stays valid across
/// sibling span pushes into the same chunk (the inferred-span case). Links/events are likewise
/// separate `Box` allocations.
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

    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
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
    let node = Box::from_raw(ptr);
    for &span in &node.spans {
        free_span_node(span);
    }
}

/// Frees a span node and its links/events.
///
/// # Safety
/// `ptr` must be a live `Box::into_raw(SpanNode)` allocation not freed elsewhere.
unsafe fn free_span_node(ptr: *mut SpanNode) {
    let node = Box::from_raw(ptr);
    for &link in &node.links {
        drop(Box::from_raw(link));
    }
    for &event in &node.events {
        drop(Box::from_raw(event));
    }
}

/// Builds a native V1 [`TracerPayloadBytes`] holding readable strings. Each node is its own heap
/// allocation (see the module docs); the builder owns the top-level chunk pointers.
#[derive(Default)]
pub struct TracerPayloadV1Builder {
    chunks: Vec<*mut ChunkNode>,
}

// SAFETY: the builder owns its node boxes and is only ever driven from a single PHP request thread,
// then consumed synchronously by the send path; the raw pointers carry no cross-thread state.
unsafe impl Send for TracerPayloadV1Builder {}

impl TracerPayloadV1Builder {
    // Index-addressed read accessors, `pub` so the PHP-side FFI adapter (`components-rs/bytes.rs`)
    // and the introspection getters can read the model back through a shared `&` borrow.
    fn chunk_node(&self, chunk: usize) -> Option<&ChunkNode> {
        // Safety: pointers in `chunks` are live `Box::into_raw` allocations owned by the builder.
        self.chunks.get(chunk).map(|&p| unsafe { &*p })
    }

    fn span_node(&self, chunk: usize, span: usize) -> Option<&SpanNode> {
        // Safety: as `chunk_node`.
        self.chunk_node(chunk)?
            .spans
            .get(span)
            .map(|&p| unsafe { &*p })
    }

    pub fn chunk(&self, chunk: usize) -> Option<&TraceChunkBytes> {
        self.chunk_node(chunk).map(|n| &n.chunk)
    }

    pub fn span(&self, chunk: usize, span: usize) -> Option<&SpanBytes> {
        self.span_node(chunk, span).map(|n| &n.span)
    }

    pub fn link(&self, chunk: usize, span: usize, link: usize) -> Option<&SpanLinkBytes> {
        // Safety: as `chunk_node`.
        self.span_node(chunk, span)?
            .links
            .get(link)
            .map(|&p| unsafe { &*p })
    }

    pub fn event(&self, chunk: usize, span: usize, event: usize) -> Option<&SpanEventBytes> {
        // Safety: as `chunk_node`.
        self.span_node(chunk, span)?
            .events
            .get(event)
            .map(|&p| unsafe { &*p })
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn span_count(&self, chunk: usize) -> usize {
        self.chunk_node(chunk).map_or(0, |c| c.spans.len())
    }

    pub fn link_count(&self, chunk: usize, span: usize) -> usize {
        self.span_node(chunk, span).map_or(0, |s| s.links.len())
    }

    pub fn event_count(&self, chunk: usize, span: usize) -> usize {
        self.span_node(chunk, span).map_or(0, |s| s.events.len())
    }

    // Index → node-pointer resolvers, used by the integration tests to reach a node the way C does
    // (they hold node pointers, not indices). Returning a copy of a stored pointer, never a deref.
    pub fn chunk_ptr(&self, chunk: usize) -> Option<*mut ChunkNode> {
        self.chunks.get(chunk).copied()
    }

    pub fn span_ptr(&self, chunk: usize, span: usize) -> Option<*mut SpanNode> {
        self.chunk_node(chunk)?.spans.get(span).copied()
    }

    pub fn link_ptr(&self, chunk: usize, span: usize, link: usize) -> Option<*mut SpanLinkBytes> {
        self.span_node(chunk, span)?.links.get(link).copied()
    }

    pub fn event_ptr(&self, chunk: usize, span: usize, event: usize) -> Option<*mut SpanEventBytes> {
        self.span_node(chunk, span)?.events.get(event).copied()
    }

    /// Appends an empty chunk with the given 128-bit trace id (high/low halves), returning a pointer
    /// to its own (heap) node.
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

    /// Consumes the builder, folding the node boxes back into the inline payload model. Dedups
    /// attribute maps here — the one finalize point before encoding — so the encoder can assume the
    /// invariant already holds.
    pub fn into_payload(mut self) -> TracerPayloadBytes {
        // Move the chunk pointers out so `Drop` (which runs at the end of this fn over the now-empty
        // `chunks`) never double-frees the nodes reclaimed below.
        let chunks = std::mem::take(&mut self.chunks);
        let mut payload = TracerPayloadBytes::default();
        for cptr in chunks {
            // Safety: `cptr` is a live `Box::into_raw` allocation, moved out of `self` and reclaimed
            // exactly once here.
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
            // Safety: every pointer in `chunks` is a live `Box::into_raw` allocation; `into_payload`
            // empties `chunks` before drop, so a node is never freed twice.
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

/// Borrows a stored `BytesString` as a short-lived `CharSlice`.
#[inline]
fn char_slice_of(field: &BytesString) -> CharSlice<'_> {
    let s = field.as_str();
    // Safety: `BytesString` guarantees valid UTF-8; the returned slice borrows from `field`, so the
    // backing allocation stays live and immutable for the slice's lifetime.
    unsafe { CharSlice::from_raw_parts(s.as_ptr().cast(), s.len()) }
}

/// Maps an attribute value to its exported [`DDOG_V1_ATTR_*`] type tag.
fn value_type(value: &AttributeValueBytes) -> u32 {
    match value {
        AttributeValueBytes::String(_) => DDOG_V1_ATTR_STRING,
        AttributeValueBytes::Int(_) => DDOG_V1_ATTR_INT,
        AttributeValueBytes::Float(_) => DDOG_V1_ATTR_DOUBLE,
        AttributeValueBytes::Bool(_) => DDOG_V1_ATTR_BOOL,
        AttributeValueBytes::Bytes(_) => DDOG_V1_ATTR_BYTES,
        AttributeValueBytes::KeyValue(_) => DDOG_V1_ATTR_KEYVALUE,
        AttributeValueBytes::List(_) => DDOG_V1_ATTR_LIST,
    }
}

// ---- Attribute-map read helpers (shared by chunk/span/link/event attr getters) ----

fn attr_count(map: &VecMap<BytesString, AttributeValueBytes>) -> usize {
    map.len()
}

fn attr_key_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> CharSlice<'_> {
    match map.iter().nth(idx) {
        Some((k, _)) => char_slice_of(k),
        None => CharSlice::empty(),
    }
}

fn attr_type_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> u32 {
    match map.iter().nth(idx) {
        Some((_, v)) => value_type(v),
        None => DDOG_V1_ATTR_STRING,
    }
}

fn attr_str_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> CharSlice<'_> {
    match map.iter().nth(idx) {
        Some((_, AttributeValueBytes::String(s))) => char_slice_of(s),
        _ => CharSlice::empty(),
    }
}

fn attr_bytes_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> CharSlice<'_> {
    match map.iter().nth(idx) {
        Some((_, AttributeValueBytes::Bytes(b))) => {
            // Safety: the returned slice borrows from `b`, which stays live and immutable for the
            // slice's lifetime.
            unsafe { CharSlice::from_raw_parts(b.as_ref().as_ptr().cast(), b.len()) }
        }
        _ => CharSlice::empty(),
    }
}

fn attr_int_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> i64 {
    match map.iter().nth(idx) {
        Some((_, AttributeValueBytes::Int(v))) => *v,
        _ => 0,
    }
}

fn attr_double_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> f64 {
    match map.iter().nth(idx) {
        Some((_, AttributeValueBytes::Float(v))) => *v,
        _ => 0.0,
    }
}

fn attr_bool_at(map: &VecMap<BytesString, AttributeValueBytes>, idx: usize) -> bool {
    matches!(
        map.iter().nth(idx),
        Some((_, AttributeValueBytes::Bool(true)))
    )
}

// ------------------- Builder lifecycle -------------------

/// Creates a new, empty V1 payload builder. Free it with [`ddog_v1_free_builder`], or hand it to
/// `ddog_send_traces_to_sidecar_v1`, which consumes it.
#[no_mangle]
pub extern "C" fn ddog_v1_new_builder() -> Box<TracerPayloadV1Builder> {
    Box::default()
}

/// Frees a V1 payload builder.
#[no_mangle]
pub extern "C" fn ddog_v1_free_builder(_builder: Box<TracerPayloadV1Builder>) {}

// ------------------- Introspection getters -------------------
//
// Shared `&TracerPayloadV1Builder`, returning scalars or builder-tied `CharSlice` borrows, so the
// payload can be read back (e.g. to rebuild the v0.4 span array in userland) without a `&mut` to C.

/// Number of chunks in the builder.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_count(builder: &TracerPayloadV1Builder) -> usize {
    builder.chunk_count()
}

/// Number of spans in `chunk`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_count(builder: &TracerPayloadV1Builder, chunk: usize) -> usize {
    builder.span_count(chunk)
}

/// Number of links on a span.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    builder.link_count(chunk, span)
}

/// Number of events on a span.
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    builder.event_count(chunk, span)
}

// ---- Chunk getters ----

/// High 64 bits of the chunk's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_trace_id_high(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> u64 {
    builder
        .chunk(chunk)
        .map_or(0, |c| trace_id_high(&c.trace_id))
}

/// Low 64 bits of the chunk's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_trace_id_low(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> u64 {
    builder
        .chunk(chunk)
        .map_or(0, |c| trace_id_low(&c.trace_id))
}

/// Reads the chunk sampling priority; returns `false` (and leaves `out` untouched) when unset.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_sampling_priority(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    out: &mut i32,
) -> bool {
    match builder.chunk(chunk).and_then(|c| c.priority) {
        Some(p) => {
            *out = p;
            true
        }
        None => false,
    }
}

/// Reads the chunk sampling mechanism; returns `false` (and leaves `out` untouched) when unset.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_sampling_mechanism(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    out: &mut u32,
) -> bool {
    match builder.chunk(chunk).and_then(|c| c.sampling_mechanism) {
        Some(m) => {
            *out = m;
            true
        }
        None => false,
    }
}

/// The chunk origin (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_origin(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> CharSlice<'_> {
    match builder.chunk(chunk) {
        Some(c) => char_slice_of(&c.origin),
        None => CharSlice::empty(),
    }
}

/// Whether the chunk is a dropped (p0) trace.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_dropped_trace(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> bool {
    builder.chunk(chunk).is_some_and(|c| c.dropped_trace)
}

/// Number of chunk-level attributes.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_attr_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> usize {
    builder
        .chunk(chunk)
        .map_or(0, |c| attr_count(&c.attributes))
}

/// Key of the chunk attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_attr_key(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.chunk(chunk) {
        Some(c) => attr_key_at(&c.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// [`DDOG_V1_ATTR_*`] type tag of the chunk attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_attr_type(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    idx: usize,
) -> u32 {
    builder
        .chunk(chunk)
        .map_or(DDOG_V1_ATTR_STRING, |c| attr_type_at(&c.attributes, idx))
}

/// String value of the chunk attribute at `idx` (empty unless it is a string).
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_attr_str(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.chunk(chunk) {
        Some(c) => attr_str_at(&c.attributes, idx),
        None => CharSlice::empty(),
    }
}

// ---- Span getters ----

/// The span service (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_service(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.service),
        None => CharSlice::empty(),
    }
}

/// The span name (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_name(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.name),
        None => CharSlice::empty(),
    }
}

/// The span resource (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_resource(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.resource),
        None => CharSlice::empty(),
    }
}

/// The span type (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_type(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.r#type),
        None => CharSlice::empty(),
    }
}

/// The span env (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_env(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.env),
        None => CharSlice::empty(),
    }
}

/// The span version (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_version(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.version),
        None => CharSlice::empty(),
    }
}

/// The span component (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_component(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => char_slice_of(&s.component),
        None => CharSlice::empty(),
    }
}

/// The span id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_id(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> u64 {
    builder.span(chunk, span).map_or(0, |s| s.span_id)
}

/// The span parent id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_parent_id(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> u64 {
    builder.span(chunk, span).map_or(0, |s| s.parent_id)
}

/// The span start time (unix nanos).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_start(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> i64 {
    builder.span(chunk, span).map_or(0, |s| s.start)
}

/// The span duration (nanos).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_duration(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> i64 {
    builder.span(chunk, span).map_or(0, |s| s.duration)
}

/// The span error flag.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_error(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> bool {
    builder.span(chunk, span).is_some_and(|s| s.error)
}

/// The span kind as its OTEL wire value.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_kind(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> u32 {
    builder
        .span(chunk, span)
        .map_or(SpanKind::default() as u32, |s| s.span_kind as u32)
}

/// Number of attributes on a span.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    builder
        .span(chunk, span)
        .map_or(0, |s| attr_count(&s.attributes))
}

/// Key of the span attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_key(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => attr_key_at(&s.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// [`DDOG_V1_ATTR_*`] type tag of the span attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_type(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> u32 {
    builder
        .span(chunk, span)
        .map_or(DDOG_V1_ATTR_STRING, |s| attr_type_at(&s.attributes, idx))
}

/// String value of the span attribute at `idx` (empty unless it is a string).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_str(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => attr_str_at(&s.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// Integer value of the span attribute at `idx` (0 unless it is an int).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_int(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> i64 {
    builder
        .span(chunk, span)
        .map_or(0, |s| attr_int_at(&s.attributes, idx))
}

/// Double value of the span attribute at `idx` (0.0 unless it is a double).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_double(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> f64 {
    builder
        .span(chunk, span)
        .map_or(0.0, |s| attr_double_at(&s.attributes, idx))
}

/// Boolean value of the span attribute at `idx` (false unless it is a true bool).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_bool(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> bool {
    builder
        .span(chunk, span)
        .is_some_and(|s| attr_bool_at(&s.attributes, idx))
}

/// Bytes value of the span attribute at `idx` (empty unless it is a bytes value).
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_attr_bytes(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.span(chunk, span) {
        Some(s) => attr_bytes_at(&s.attributes, idx),
        None => CharSlice::empty(),
    }
}

// ---- Link getters ----

/// High 64 bits of the link's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_trace_id_high(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> u64 {
    builder
        .link(chunk, span, link)
        .map_or(0, |l| trace_id_high(&l.trace_id))
}

/// Low 64 bits of the link's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_trace_id_low(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> u64 {
    builder
        .link(chunk, span, link)
        .map_or(0, |l| trace_id_low(&l.trace_id))
}

/// The link span id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_span_id(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> u64 {
    builder.link(chunk, span, link).map_or(0, |l| l.span_id)
}

/// The link flags.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_flags(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> u32 {
    builder.link(chunk, span, link).map_or(0, |l| l.flags)
}

/// The link tracestate (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_tracestate(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> CharSlice<'_> {
    match builder.link(chunk, span, link) {
        Some(l) => char_slice_of(&l.tracestate),
        None => CharSlice::empty(),
    }
}

/// Number of attributes on a link.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_attr_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
) -> usize {
    builder
        .link(chunk, span, link)
        .map_or(0, |l| attr_count(&l.attributes))
}

/// Key of the link attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_attr_key(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.link(chunk, span, link) {
        Some(l) => attr_key_at(&l.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// String value of the link attribute at `idx` (empty unless it is a string).
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_attr_str(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.link(chunk, span, link) {
        Some(l) => attr_str_at(&l.attributes, idx),
        None => CharSlice::empty(),
    }
}

// ---- Event getters ----

/// The event time (unix nanos).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_time(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
) -> u64 {
    builder
        .event(chunk, span, event)
        .map_or(0, |e| e.time_unix_nano)
}

/// The event name (empty if unset).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_name(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
) -> CharSlice<'_> {
    match builder.event(chunk, span, event) {
        Some(e) => char_slice_of(&e.name),
        None => CharSlice::empty(),
    }
}

/// Number of attributes on an event.
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
) -> usize {
    builder
        .event(chunk, span, event)
        .map_or(0, |e| attr_count(&e.attributes))
}

/// Key of the event attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_key(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.event(chunk, span, event) {
        Some(e) => attr_key_at(&e.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// [`DDOG_V1_ATTR_*`] type tag of the event attribute at `idx`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_type(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> u32 {
    builder
        .event(chunk, span, event)
        .map_or(DDOG_V1_ATTR_STRING, |e| attr_type_at(&e.attributes, idx))
}

/// String value of the event attribute at `idx` (empty unless it is a string).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_str(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> CharSlice<'_> {
    match builder.event(chunk, span, event) {
        Some(e) => attr_str_at(&e.attributes, idx),
        None => CharSlice::empty(),
    }
}

/// Integer value of the event attribute at `idx` (0 unless it is an int).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_int(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> i64 {
    builder
        .event(chunk, span, event)
        .map_or(0, |e| attr_int_at(&e.attributes, idx))
}

/// Double value of the event attribute at `idx` (0.0 unless it is a double).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_double(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> f64 {
    builder
        .event(chunk, span, event)
        .map_or(0.0, |e| attr_double_at(&e.attributes, idx))
}

/// Boolean value of the event attribute at `idx` (false unless it is a true bool).
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_attr_bool(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    idx: usize,
) -> bool {
    builder
        .event(chunk, span, event)
        .is_some_and(|e| attr_bool_at(&e.attributes, idx))
}

// ------------------- Payload-level metadata -------------------

/// Populates payload-level metadata from send-time values. Empty strings are omitted;
/// `git_commit_sha` becomes the payload attribute `_dd.git.commit.sha`.
#[allow(clippy::too_many_arguments)]
pub fn populate_payload_metadata(
    payload: &mut TracerPayloadBytes,
    container_id: &str,
    language_name: &str,
    language_version: &str,
    tracer_version: &str,
    runtime_id: &str,
    env: &str,
    hostname: &str,
    app_version: &str,
    git_commit_sha: &str,
) {
    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap_or_default()
    }
    payload.container_id = bs(container_id);
    payload.language_name = bs(language_name);
    payload.language_version = bs(language_version);
    payload.tracer_version = bs(tracer_version);
    payload.runtime_id = bs(runtime_id);
    payload.env = bs(env);
    payload.hostname = bs(hostname);
    payload.app_version = bs(app_version);
    if !git_commit_sha.is_empty() {
        payload.attributes.insert(
            BytesString::from_static("_dd.git.commit.sha"),
            AttributeValueBytes::String(bs(git_commit_sha)),
        );
    }
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
/// chunk/span node pointers (the outer frame's still-live handles), so it works mid-build before the
/// nodes are folded into the inline payload. The returned owned slice must be freed with
/// [`ddog_free_charslice`].
///
/// # Safety
/// `chunk`/`span` must be live node pointers previously returned by `ddog_new_chunk`/`ddog_new_span`
/// (with `span` a span of `chunk`).
#[no_mangle]
pub unsafe extern "C" fn ddog_v1_span_debug_log(
    chunk: *mut ChunkNode,
    span: *mut SpanNode,
) -> CharSlice<'static> {
    let chunk_node = &*chunk;
    let span_node = &*span;
    let debug_str = render_span_debug(
        span_node.span(),
        Some(&chunk_node.chunk),
        span_node.link_count(),
        span_node.event_count(),
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
#[no_mangle]
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
