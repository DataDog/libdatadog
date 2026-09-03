// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Index-based FFI builder for the native V1 trace payload
//! ([`libdd_trace_utils::span::v1::TracerPayload`]).
//!
//! This builder stores **real, readable strings** ([`BytesString`]) directly on each
//! [`SpanBytes`]/[`TraceChunkBytes`]/[`TracerPayloadBytes`] field, mirroring the v0.4 builder in
//! [`crate::span`]. Setters take a [`CharSlice`] and store its content verbatim; getters hand back
//! short-lived [`CharSlice`] borrows over the stored strings. There is no build-time string interning
//! table: wire-level string deduplication is performed independently by the V1 encoder's own
//! streaming table at encode time, so an in-memory intern table would only add cost while blocking
//! introspection of the built payload.
//!
//! The builder is index-based rather than pointer-based (chunks/spans/links/events are addressed by
//! their `usize` position, not by a returned `&mut` handle). This is deliberate: every mutating
//! operation is routed through a single `&mut TracerPayloadV1Builder`, so we never hand a `&mut`
//! into the payload out to C while the builder is still live (which would alias under Stacked
//! Borrows). Read-only getters take a shared `&TracerPayloadV1Builder` and return borrows tied to
//! it, which is likewise sound.
//!
//! Payload-level metadata (container id, language, tracer version, runtime id, env, hostname,
//! app version, git commit sha) is NOT set through this builder: it is applied at send time from
//! `TracerMetadataV1` + the sender's `tracer_headers_tags` (see
//! [`crate::populate_payload_metadata`]).

use libdd_common_ffi::slice::CharSlice;
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v1::{
    AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes, TraceChunkBytes,
    TracerPayloadBytes,
};
use libdd_trace_utils::span::vec_map::VecMap;
use std::ffi::CString;
use std::fmt::Write as _;

/// Attribute value type tags returned by the `ddog_v1_get_*_attr_type` getters. They let a C caller
/// pick the matching typed value getter (`_attr_str`/`_attr_int`/`_attr_double`/`_attr_bool`/
/// `_attr_bytes`) for a given attribute index.
pub const DDOG_V1_ATTR_STRING: u32 = 0;
pub const DDOG_V1_ATTR_INT: u32 = 1;
pub const DDOG_V1_ATTR_DOUBLE: u32 = 2;
pub const DDOG_V1_ATTR_BOOL: u32 = 3;
pub const DDOG_V1_ATTR_BYTES: u32 = 4;
pub const DDOG_V1_ATTR_KEYVALUE: u32 = 5;
pub const DDOG_V1_ATTR_LIST: u32 = 6;

/// Builds a native V1 [`TracerPayloadBytes`] holding readable strings.
#[derive(Default)]
pub struct TracerPayloadV1Builder {
    /// The native V1 payload being assembled. Only chunks/spans are populated here; payload-level
    /// metadata is applied at send time.
    payload: TracerPayloadBytes,
}

impl TracerPayloadV1Builder {
    // Index-addressed accessors, `pub` so the PHP-side FFI adapter (`components-rs/bytes.rs`) can
    // fill the model without any `#[no_mangle]` mutator C-ABI and without a live `&mut` ever escaping
    // to C: every mutation routes through a single `&mut TracerPayloadV1Builder` per call.
    pub fn chunk(&self, chunk: usize) -> Option<&TraceChunkBytes> {
        self.payload.chunks.get(chunk)
    }

    pub fn chunk_mut(&mut self, chunk: usize) -> Option<&mut TraceChunkBytes> {
        self.payload.chunks.get_mut(chunk)
    }

    pub fn span(&self, chunk: usize, span: usize) -> Option<&SpanBytes> {
        self.payload.chunks.get(chunk)?.spans.get(span)
    }

    pub fn span_mut(&mut self, chunk: usize, span: usize) -> Option<&mut SpanBytes> {
        self.payload.chunks.get_mut(chunk)?.spans.get_mut(span)
    }

    pub fn link(&self, chunk: usize, span: usize, link: usize) -> Option<&SpanLinkBytes> {
        self.span(chunk, span)?.span_links.get(link)
    }

    pub fn link_mut(
        &mut self,
        chunk: usize,
        span: usize,
        link: usize,
    ) -> Option<&mut SpanLinkBytes> {
        self.span_mut(chunk, span)?.span_links.get_mut(link)
    }

    pub fn event(&self, chunk: usize, span: usize, event: usize) -> Option<&SpanEventBytes> {
        self.span(chunk, span)?.span_events.get(event)
    }

    pub fn event_mut(
        &mut self,
        chunk: usize,
        span: usize,
        event: usize,
    ) -> Option<&mut SpanEventBytes> {
        self.span_mut(chunk, span)?.span_events.get_mut(event)
    }

    /// Appends an empty chunk with the given 128-bit trace id (high/low halves), returning its index.
    pub fn push_chunk(&mut self, trace_id_high: u64, trace_id_low: u64) -> usize {
        self.payload.chunks.push(TraceChunkBytes {
            trace_id: trace_id_bytes(trace_id_high, trace_id_low),
            ..Default::default()
        });
        self.payload.chunks.len() - 1
    }

    /// Appends an empty span to `chunk`, returning its index (0 if the chunk is out of range).
    pub fn push_span(&mut self, chunk: usize) -> usize {
        match self.chunk_mut(chunk) {
            Some(c) => {
                c.spans.push(SpanBytes::default());
                c.spans.len() - 1
            }
            None => 0,
        }
    }

    /// Appends an empty link to a span, returning its index (0 if the span is out of range).
    pub fn push_link(&mut self, chunk: usize, span: usize) -> usize {
        match self.span_mut(chunk, span) {
            Some(s) => {
                s.span_links.push(SpanLinkBytes::default());
                s.span_links.len() - 1
            }
            None => 0,
        }
    }

    /// Appends an empty event to a span, returning its index (0 if the span is out of range).
    pub fn push_event(&mut self, chunk: usize, span: usize) -> usize {
        match self.span_mut(chunk, span) {
            Some(s) => {
                s.span_events.push(SpanEventBytes::default());
                s.span_events.len() - 1
            }
            None => 0,
        }
    }

    /// Consumes the builder, returning the assembled payload (chunks/spans only). Attribute maps
    /// are deduped here — the single finalize point before encoding — so the encoder finds the
    /// deduped invariant already satisfied (no per-encode on-the-fly dedup or warning).
    pub fn into_payload(mut self) -> TracerPayloadBytes {
        self.payload.dedup();
        self.payload
    }
}

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
    matches!(map.iter().nth(idx), Some((_, AttributeValueBytes::Bool(true))))
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
// All getters take a shared `&TracerPayloadV1Builder` and return either scalars or `CharSlice`
// borrows tied to the builder, so the built payload can be read back (e.g. to reconstruct the
// classic v0.4 span array in userland) without ever handing a `&mut` into the payload to C.

/// Number of chunks in the builder.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_count(builder: &TracerPayloadV1Builder) -> usize {
    builder.payload.chunks.len()
}

/// Number of spans in `chunk`.
#[no_mangle]
pub extern "C" fn ddog_v1_get_span_count(builder: &TracerPayloadV1Builder, chunk: usize) -> usize {
    builder.chunk(chunk).map_or(0, |c| c.spans.len())
}

/// Number of links on a span.
#[no_mangle]
pub extern "C" fn ddog_v1_get_link_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    builder.span(chunk, span).map_or(0, |s| s.span_links.len())
}

/// Number of events on a span.
#[no_mangle]
pub extern "C" fn ddog_v1_get_event_count(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    builder.span(chunk, span).map_or(0, |s| s.span_events.len())
}

// ---- Chunk getters ----

/// High 64 bits of the chunk's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_trace_id_high(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> u64 {
    builder.chunk(chunk).map_or(0, |c| trace_id_high(&c.trace_id))
}

/// Low 64 bits of the chunk's 128-bit trace id.
#[no_mangle]
pub extern "C" fn ddog_v1_get_chunk_trace_id_low(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
) -> u64 {
    builder.chunk(chunk).map_or(0, |c| trace_id_low(&c.trace_id))
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
    builder.chunk(chunk).map_or(0, |c| attr_count(&c.attributes))
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

/// Populates the payload-level metadata fields consumed by the V1 encoder from values sourced at
/// send time (`TracerMetadataV1` + the sender's `tracer_headers_tags`). Empty strings are left as
/// the default and omitted by the encoder. `git_commit_sha`, when present, is emitted as the
/// payload attribute `_dd.git.commit.sha`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn populate_payload_metadata(
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

/// Renders a V1 span (plus its owning chunk's trace id) as the readable diagnostic string, covering
/// service/name/resource/type, ids, timestamps, error/kind, the promoted fields (env/version/
/// component), typed attributes, and link/event counts.
fn render_span_debug(span: &SpanBytes, chunk: Option<&TraceChunkBytes>) -> String {
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
        span.span_links.len(),
        span.span_events.len(),
    );
    out
}

/// Renders the span at index `chunk`/`span` as a human-readable diagnostic string, used by
/// dd-trace-php to emit the `DD_TRACE_DEBUG` "[span] Encoding span: …" line on the V1 path. It is
/// index-addressed (the V1 builder never hands out `&mut`/`&` span handles to C); an out-of-range
/// index yields an empty slice.
///
/// The returned slice is an owned allocation that must be freed with the very same free function as
/// the v0.4 variant, [`crate::span::ddog_free_charslice`].
#[no_mangle]
pub extern "C" fn ddog_v1_span_debug_log(
    builder: &TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> CharSlice<'static> {
    let debug_str = match builder.span(chunk, span) {
        Some(s) => render_span_debug(s, builder.chunk(chunk)),
        None => String::new(),
    };
    let len = debug_str.len();
    let cstring = CString::new(debug_str).unwrap_or_default();

    // Safety: `CString` is an owned, valid UTF-8 string; the pointer is freed by
    // `ddog_free_charslice`.
    unsafe { CharSlice::from_raw_parts(cstring.into_raw().cast(), len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common_ffi::slice::AsBytes;
    use libdd_tinybytes::Bytes;
    use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
    use std::borrow::Cow;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    // Test-only builder helpers. The production `ddog_v1_*` mutator C-ABI was removed in the
    // bytes.rs V1 pivot (the PHP-side FFI fills the model through the pub `TracerPayloadV1Builder`
    // methods instead). These thin fns reproduce that removed surface so the getter/encoder/
    // debug-log coverage below keeps exercising a fully populated payload.

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

    fn ddog_v1_set_chunk_dropped_trace(b: &mut TracerPayloadV1Builder, chunk: usize, dropped: bool) {
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
    fn ddog_v1_set_span_component(b: &mut TracerPayloadV1Builder, c: usize, s: usize, v: CharSlice) {
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
        add_span_attr(b, c, s, key, AttributeValueBytes::String(to_bytes_string(value)));
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
        b.span(c, s).is_some_and(|sp| sp.attributes.contains_key(&key))
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
        assert_eq!(ddog_v1_get_chunk_attr_key(&b, ci, 0).to_utf8_lossy(), "c_key");
        assert_eq!(ddog_v1_get_chunk_attr_type(&b, ci, 0), DDOG_V1_ATTR_STRING);
        assert_eq!(ddog_v1_get_chunk_attr_str(&b, ci, 0).to_utf8_lossy(), "c_val");

        // Span getters.
        assert_eq!(ddog_v1_get_span_count(&b, ci), 1);
        assert_eq!(ddog_v1_get_span_service(&b, ci, si).to_utf8_lossy(), "svc");
        assert_eq!(ddog_v1_get_span_name(&b, ci, si).to_utf8_lossy(), "op");
        assert_eq!(ddog_v1_get_span_resource(&b, ci, si).to_utf8_lossy(), "res");
        assert_eq!(ddog_v1_get_span_type(&b, ci, si).to_utf8_lossy(), "web");
        assert_eq!(ddog_v1_get_span_env(&b, ci, si).to_utf8_lossy(), "prod");
        assert_eq!(ddog_v1_get_span_version(&b, ci, si).to_utf8_lossy(), "1.2.3");
        assert_eq!(ddog_v1_get_span_component(&b, ci, si).to_utf8_lossy(), "pdo");
        assert_eq!(ddog_v1_get_span_id(&b, ci, si), 42);
        assert_eq!(ddog_v1_get_span_parent_id(&b, ci, si), 7);
        assert_eq!(ddog_v1_get_span_start(&b, ci, si), 1_000);
        assert_eq!(ddog_v1_get_span_duration(&b, ci, si), 500);
        assert!(ddog_v1_get_span_error(&b, ci, si));
        assert_eq!(ddog_v1_get_span_kind(&b, ci, si), 3);

        // Span attributes: 5 typed values in insertion order.
        assert_eq!(ddog_v1_get_span_attr_count(&b, ci, si), 5);
        assert_eq!(ddog_v1_get_span_attr_key(&b, ci, si, 0).to_utf8_lossy(), "a_str");
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 0), DDOG_V1_ATTR_STRING);
        assert_eq!(ddog_v1_get_span_attr_str(&b, ci, si, 0).to_utf8_lossy(), "v");
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 1), DDOG_V1_ATTR_INT);
        assert_eq!(ddog_v1_get_span_attr_int(&b, ci, si, 1), 11);
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 2), DDOG_V1_ATTR_DOUBLE);
        assert_eq!(ddog_v1_get_span_attr_double(&b, ci, si, 2), 1.5);
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 3), DDOG_V1_ATTR_BOOL);
        assert!(ddog_v1_get_span_attr_bool(&b, ci, si, 3));
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, si, 4), DDOG_V1_ATTR_BYTES);
        assert_eq!(ddog_v1_get_span_attr_bytes(&b, ci, si, 4).to_utf8_lossy(), "raw");

        // Link getters.
        assert_eq!(ddog_v1_get_link_count(&b, ci, si), 1);
        assert_eq!(ddog_v1_get_link_trace_id_high(&b, ci, si, li), 0x11);
        assert_eq!(ddog_v1_get_link_trace_id_low(&b, ci, si, li), 0x22);
        assert_eq!(ddog_v1_get_link_span_id(&b, ci, si, li), 9);
        assert_eq!(ddog_v1_get_link_flags(&b, ci, si, li), 1);
        assert_eq!(ddog_v1_get_link_tracestate(&b, ci, si, li).to_utf8_lossy(), "dd=s:1");
        assert_eq!(ddog_v1_get_link_attr_count(&b, ci, si, li), 1);
        assert_eq!(ddog_v1_get_link_attr_key(&b, ci, si, li, 0).to_utf8_lossy(), "l_key");
        assert_eq!(ddog_v1_get_link_attr_str(&b, ci, si, li, 0).to_utf8_lossy(), "l_val");

        // Event getters.
        assert_eq!(ddog_v1_get_event_count(&b, ci, si), 1);
        assert_eq!(ddog_v1_get_event_time(&b, ci, si, evi), 123);
        assert_eq!(ddog_v1_get_event_name(&b, ci, si, evi).to_utf8_lossy(), "exception");
        assert_eq!(ddog_v1_get_event_attr_count(&b, ci, si, evi), 1);
        assert_eq!(ddog_v1_get_event_attr_key(&b, ci, si, evi, 0).to_utf8_lossy(), "e_int");
        assert_eq!(ddog_v1_get_event_attr_type(&b, ci, si, evi, 0), DDOG_V1_ATTR_INT);
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
        assert_eq!(ddog_v1_get_span_attr_key(&b, ci, inferred, 0).to_utf8_lossy(), "error.message");
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, inferred, 0), DDOG_V1_ATTR_STRING);
        assert_eq!(ddog_v1_get_span_attr_str(&b, ci, inferred, 0).to_utf8_lossy(), "boom");

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
        assert_eq!(ddog_v1_get_span_attr_type(&b, ci, inferred, 1), DDOG_V1_ATTR_DOUBLE);
        assert_eq!(ddog_v1_get_span_attr_double(&b, ci, inferred, 1), 0.5);

        // absent source key is a no-op (destination untouched, nothing deleted).
        assert!(!ddog_v1_transfer_span_attr(&mut b, ci, root, inferred, cs("missing"), true));
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
        assert!(rendered.contains("my-service"), "service should appear: {rendered}");
        assert!(rendered.contains("my-operation"), "name should appear: {rendered}");
        assert!(rendered.contains("resource=\"GET /x\""), "resource should appear: {rendered}");
        assert!(rendered.contains("span_id=42"));
        assert!(rendered.contains("parent_id=7"));
        assert!(rendered.contains("error=true"));
        assert!(rendered.contains("kind=Server"));
        assert!(rendered.contains("component=\"pdo\""));
        assert!(rendered.contains("http.status_code=200"));
        assert!(rendered.contains("links=1"));
        assert!(rendered.contains("events=1"));
        // Frees correctly via the same free function as the v0.4 variant.
        unsafe { crate::span::ddog_free_charslice(slice) };

        // Out-of-range index yields an empty (safely freeable) slice.
        let empty = ddog_v1_span_debug_log(&b, 99, 0);
        assert!(empty.to_utf8_lossy().is_empty());
        unsafe { crate::span::ddog_free_charslice(empty) };
    }
}
