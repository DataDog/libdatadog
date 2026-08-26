// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Index-based FFI builder for the native V1 trace payload
//! ([`libdd_trace_utils::span::v1::TracerPayload`]).
//!
//! Unlike the v0.4 builder in [`crate::span`] — which stores strings inline on each span — this
//! builder owns a **value-keyed intern table**. The C caller interns each string once via
//! [`ddog_v1_intern_string`], gets back a stable `u32` id, and references strings by id in every
//! subsequent call. This lets the caller keep its own `zend_string*`→id cache for the per-pointer
//! fast path without weakening deduplication: equal strings always map to the same id (and thus a
//! single [`BytesString`] allocation).
//!
//! The builder is index-based rather than pointer-based (chunks/spans/links/events are addressed by
//! their `usize` position, not by a returned `&mut` handle). This is deliberate: interning mutates
//! the builder, so handing out `&mut` handles into the same builder that `ddog_v1_intern_string`
//! later mutates would alias under Stacked Borrows. Addressing by index routes every operation
//! through a single `&mut TracerPayloadV1Builder`, which is sound and lets us build the native
//! [`TracerPayloadBytes`] directly (no intermediate id-typed model). Wire-level string
//! deduplication is still performed independently by the encoder's own streaming table at encode
//! time; the intern table here only dedups the in-memory builder representation.
//!
//! Payload-level metadata (container id, language, tracer version, runtime id, env, hostname,
//! app version, git commit sha) is NOT set through this builder: it is applied at send time from
//! `TracerMetadataV1` + the sender's `tracer_headers_tags` (see
//! [`crate::populate_payload_metadata`]).

use libdd_common_ffi::slice::{AsBytes, CharSlice};
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_utils::span::v1::{
    AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes, TraceChunkBytes,
    TracerPayloadBytes,
};
use libdd_trace_utils::span::vec_map::VecMap;
use std::collections::HashMap;

/// Builds a native V1 [`TracerPayloadBytes`] while interning every string once.
pub struct TracerPayloadV1Builder {
    /// Value-keyed intern table: string content → id. Guarantees equal strings share one id.
    intern_map: HashMap<String, u32>,
    /// id → interned string. `strings[0]` is always the empty string (id 0).
    strings: Vec<BytesString>,
    /// The native V1 payload being assembled. Only chunks/spans are populated here; payload-level
    /// metadata is applied at send time.
    payload: TracerPayloadBytes,
}

impl Default for TracerPayloadV1Builder {
    fn default() -> Self {
        let mut intern_map = HashMap::new();
        intern_map.insert(String::new(), 0);
        Self {
            intern_map,
            strings: vec![BytesString::default()],
            payload: TracerPayloadBytes::default(),
        }
    }
}

impl TracerPayloadV1Builder {
    /// Interns `slice`, returning its stable id. The empty string is always id 0.
    fn intern(&mut self, slice: CharSlice) -> u32 {
        if slice.is_empty() {
            return 0;
        }
        let s = String::from_utf8_lossy(slice.as_bytes()).into_owned();
        if let Some(&id) = self.intern_map.get(&s) {
            return id;
        }
        let id = self.strings.len() as u32;
        self.strings.push(BytesString::from_string(s.clone()));
        self.intern_map.insert(s, id);
        id
    }

    /// Resolves an id to its interned string. Out-of-range ids resolve to the empty string.
    fn resolve(&self, id: u32) -> BytesString {
        self.strings.get(id as usize).cloned().unwrap_or_default()
    }

    fn chunk_mut(&mut self, chunk: usize) -> Option<&mut TraceChunkBytes> {
        self.payload.chunks.get_mut(chunk)
    }

    fn span_mut(&mut self, chunk: usize, span: usize) -> Option<&mut SpanBytes> {
        self.payload.chunks.get_mut(chunk)?.spans.get_mut(span)
    }

    fn link_mut(&mut self, chunk: usize, span: usize, link: usize) -> Option<&mut SpanLinkBytes> {
        self.span_mut(chunk, span)?.span_links.get_mut(link)
    }

    fn event_mut(
        &mut self,
        chunk: usize,
        span: usize,
        event: usize,
    ) -> Option<&mut SpanEventBytes> {
        self.span_mut(chunk, span)?.span_events.get_mut(event)
    }

    /// Consumes the builder, returning the assembled payload (chunks/spans only).
    pub fn into_payload(self) -> TracerPayloadBytes {
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

fn insert_attr(
    map: &mut VecMap<BytesString, AttributeValueBytes>,
    key: BytesString,
    value: AttributeValueBytes,
) {
    if key.as_str().is_empty() {
        return;
    }
    map.insert(key, value);
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

/// Interns `string` into the builder's value-keyed table and returns its stable id. Equal strings
/// (including across chunks/spans) always return the same id; the empty string is always id 0.
#[no_mangle]
pub extern "C" fn ddog_v1_intern_string(
    builder: &mut TracerPayloadV1Builder,
    string: CharSlice,
) -> u32 {
    builder.intern(string)
}

// ------------------- Chunk construction -------------------

/// Appends a new (empty) chunk with the given 128-bit trace id, returning its index.
///
/// A chunk must be fully built (all its spans/links/events) before the next chunk is created:
/// creating a chunk may reallocate the chunk vector and invalidate positions cached as raw
/// pointers. Indices remain valid.
#[no_mangle]
pub extern "C" fn ddog_v1_builder_new_chunk(
    builder: &mut TracerPayloadV1Builder,
    trace_id_high: u64,
    trace_id_low: u64,
) -> usize {
    builder.payload.chunks.push(TraceChunkBytes {
        trace_id: trace_id_bytes(trace_id_high, trace_id_low),
        ..Default::default()
    });
    builder.payload.chunks.len() - 1
}

/// Sets the chunk sampling priority (v0.4 `_sampling_priority_v1`).
#[no_mangle]
pub extern "C" fn ddog_v1_set_chunk_sampling_priority(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    priority: i32,
) {
    if let Some(c) = builder.chunk_mut(chunk) {
        c.priority = Some(priority);
    }
}

/// Sets the chunk origin (v0.4 `_dd.origin`) from an interned id.
#[no_mangle]
pub extern "C" fn ddog_v1_set_chunk_origin(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    origin_id: u32,
) {
    let origin = builder.resolve(origin_id);
    if let Some(c) = builder.chunk_mut(chunk) {
        c.origin = origin;
    }
}

/// Sets the chunk sampling mechanism (v0.4 `_dd.p.dm`).
#[no_mangle]
pub extern "C" fn ddog_v1_set_chunk_sampling_mechanism(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    mechanism: u32,
) {
    if let Some(c) = builder.chunk_mut(chunk) {
        c.sampling_mechanism = Some(mechanism);
    }
}

/// Marks the chunk as a dropped (p0) trace.
#[no_mangle]
pub extern "C" fn ddog_v1_set_chunk_dropped_trace(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    dropped: bool,
) {
    if let Some(c) = builder.chunk_mut(chunk) {
        c.dropped_trace = dropped;
    }
}

/// Adds a string-valued chunk-level attribute (key and value are interned ids).
#[no_mangle]
pub extern "C" fn ddog_v1_add_chunk_attr_str(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    key_id: u32,
    value_id: u32,
) {
    let key = builder.resolve(key_id);
    let value = builder.resolve(value_id);
    if let Some(c) = builder.chunk_mut(chunk) {
        insert_attr(&mut c.attributes, key, AttributeValueBytes::String(value));
    }
}

// ------------------- Span construction -------------------

/// Appends a new (empty) span to `chunk`, returning its index within that chunk.
///
/// A span must be fully built before the next span is created in the same chunk.
#[no_mangle]
pub extern "C" fn ddog_v1_chunk_new_span(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
) -> usize {
    match builder.chunk_mut(chunk) {
        Some(c) => {
            c.spans.push(SpanBytes::default());
            c.spans.len() - 1
        }
        None => 0,
    }
}

/// Sets the span service (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_service(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.service = value;
    }
}

/// Sets the span name (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_name(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.name = value;
    }
}

/// Sets the span resource (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_resource(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.resource = value;
    }
}

/// Sets the span type (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_type(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.r#type = value;
    }
}

/// Sets the span env (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_env(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.env = value;
    }
}

/// Sets the span version (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_version(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.version = value;
    }
}

/// Sets the span component (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_component(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(s) = builder.span_mut(chunk, span) {
        s.component = value;
    }
}

/// Sets the span id.
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_id(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    value: u64,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.span_id = value;
    }
}

/// Sets the span parent id.
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_parent_id(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    value: u64,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.parent_id = value;
    }
}

/// Sets the span start time (unix nanos; negative values are normalized at encode time).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_start(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    value: i64,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.start = value;
    }
}

/// Sets the span duration (nanos).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_duration(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    value: i64,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.duration = value;
    }
}

/// Sets the span error flag.
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_error(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    error: bool,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.error = error;
    }
}

/// Sets the span kind (OTEL wire value; unset/unknown → Internal).
#[no_mangle]
pub extern "C" fn ddog_v1_set_span_kind(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    kind: u32,
) {
    if let Some(s) = builder.span_mut(chunk, span) {
        s.span_kind = SpanKind::from(kind);
    }
}

/// Adds a string span attribute (key and value are interned ids).
#[no_mangle]
pub extern "C" fn ddog_v1_add_span_attr_str(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    key_id: u32,
    value: u32,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::String(builder.resolve(value));
    if let Some(s) = builder.span_mut(chunk, span) {
        insert_attr(&mut s.attributes, key, attr);
    }
}

/// Adds an integer span attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_span_attr_int(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    key_id: u32,
    value: i64,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Int(value);
    if let Some(s) = builder.span_mut(chunk, span) {
        insert_attr(&mut s.attributes, key, attr);
    }
}

/// Adds a double span attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_span_attr_double(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    key_id: u32,
    value: f64,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Float(value);
    if let Some(s) = builder.span_mut(chunk, span) {
        insert_attr(&mut s.attributes, key, attr);
    }
}

/// Adds a boolean span attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_span_attr_bool(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    key_id: u32,
    value: bool,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Bool(value);
    if let Some(s) = builder.span_mut(chunk, span) {
        insert_attr(&mut s.attributes, key, attr);
    }
}

/// Adds a bytes-valued span attribute. The key is an interned id; the value bytes are copied
/// verbatim (not interned) and encoded as msgpack `bin`.
#[no_mangle]
pub extern "C" fn ddog_v1_add_span_attr_bytes(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    key_id: u32,
    value: CharSlice,
) {
    let key = builder.resolve(key_id);
    let bytes = Bytes::copy_from_slice(value.as_bytes());
    if let Some(s) = builder.span_mut(chunk, span) {
        insert_attr(&mut s.attributes, key, AttributeValueBytes::Bytes(bytes));
    }
}

// ------------------- Span link construction -------------------

/// Appends a new (empty) link to a span, returning its index within that span.
#[no_mangle]
pub extern "C" fn ddog_v1_span_new_link(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    match builder.span_mut(chunk, span) {
        Some(s) => {
            s.span_links.push(SpanLinkBytes::default());
            s.span_links.len() - 1
        }
        None => 0,
    }
}

/// Sets the link's 128-bit trace id (high/low halves).
#[no_mangle]
pub extern "C" fn ddog_v1_set_link_trace_id(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    trace_id_high: u64,
    trace_id_low: u64,
) {
    if let Some(l) = builder.link_mut(chunk, span, link) {
        l.trace_id = trace_id_bytes(trace_id_high, trace_id_low);
    }
}

/// Sets the link span id.
#[no_mangle]
pub extern "C" fn ddog_v1_set_link_span_id(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    value: u64,
) {
    if let Some(l) = builder.link_mut(chunk, span, link) {
        l.span_id = value;
    }
}

/// Sets the link flags (W3C trace-flags plus the "set" sentinel bit).
#[no_mangle]
pub extern "C" fn ddog_v1_set_link_flags(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    value: u32,
) {
    if let Some(l) = builder.link_mut(chunk, span, link) {
        l.flags = value;
    }
}

/// Sets the link tracestate (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_link_tracestate(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(l) = builder.link_mut(chunk, span, link) {
        l.tracestate = value;
    }
}

/// Adds a string-valued link attribute (key and value are interned ids).
#[no_mangle]
pub extern "C" fn ddog_v1_add_link_attr_str(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    link: usize,
    key_id: u32,
    value_id: u32,
) {
    let key = builder.resolve(key_id);
    let value = builder.resolve(value_id);
    if let Some(l) = builder.link_mut(chunk, span, link) {
        insert_attr(&mut l.attributes, key, AttributeValueBytes::String(value));
    }
}

// ------------------- Span event construction -------------------

/// Appends a new (empty) event to a span, returning its index within that span.
#[no_mangle]
pub extern "C" fn ddog_v1_span_new_event(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
) -> usize {
    match builder.span_mut(chunk, span) {
        Some(s) => {
            s.span_events.push(SpanEventBytes::default());
            s.span_events.len() - 1
        }
        None => 0,
    }
}

/// Sets the event time (unix nanos).
#[no_mangle]
pub extern "C" fn ddog_v1_set_event_time(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    time_unix_nano: u64,
) {
    if let Some(e) = builder.event_mut(chunk, span, event) {
        e.time_unix_nano = time_unix_nano;
    }
}

/// Sets the event name (interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_set_event_name(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    id: u32,
) {
    let value = builder.resolve(id);
    if let Some(e) = builder.event_mut(chunk, span, event) {
        e.name = value;
    }
}

/// Adds a string event attribute (key and value are interned ids).
#[no_mangle]
pub extern "C" fn ddog_v1_add_event_attr_str(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    key_id: u32,
    value: u32,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::String(builder.resolve(value));
    if let Some(e) = builder.event_mut(chunk, span, event) {
        insert_attr(&mut e.attributes, key, attr);
    }
}

/// Adds an integer event attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_event_attr_int(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    key_id: u32,
    value: i64,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Int(value);
    if let Some(e) = builder.event_mut(chunk, span, event) {
        insert_attr(&mut e.attributes, key, attr);
    }
}

/// Adds a double event attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_event_attr_double(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    key_id: u32,
    value: f64,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Float(value);
    if let Some(e) = builder.event_mut(chunk, span, event) {
        insert_attr(&mut e.attributes, key, attr);
    }
}

/// Adds a boolean event attribute (key is an interned id).
#[no_mangle]
pub extern "C" fn ddog_v1_add_event_attr_bool(
    builder: &mut TracerPayloadV1Builder,
    chunk: usize,
    span: usize,
    event: usize,
    key_id: u32,
    value: bool,
) {
    let key = builder.resolve(key_id);
    let attr = AttributeValueBytes::Bool(value);
    if let Some(e) = builder.event_mut(chunk, span, event) {
        insert_attr(&mut e.attributes, key, attr);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn intern_empty_is_zero_and_dedups() {
        let mut b = TracerPayloadV1Builder::default();
        assert_eq!(b.intern(CharSlice::empty()), 0);
        assert_eq!(b.intern(cs("")), 0);
        let a = b.intern(cs("svc"));
        let a2 = b.intern(cs("svc"));
        let c = b.intern(cs("other"));
        assert_eq!(a, a2, "equal strings must share an id");
        assert_ne!(a, c);
        assert_eq!(a, 1);
        assert_eq!(c, 2);
        // id 0 always resolves to empty.
        assert!(b.resolve(0).as_str().is_empty());
        assert_eq!(b.resolve(a).as_str(), "svc");
        // out-of-range id resolves to empty.
        assert!(b.resolve(999).as_str().is_empty());
    }

    #[test]
    fn builds_span_with_promoted_and_typed_attributes() {
        let mut b = TracerPayloadV1Builder::default();
        let svc = ddog_v1_intern_string(&mut b, cs("svc"));
        let name = ddog_v1_intern_string(&mut b, cs("op"));
        let res = ddog_v1_intern_string(&mut b, cs("res"));
        let kstr = ddog_v1_intern_string(&mut b, cs("k_str"));
        let vstr = ddog_v1_intern_string(&mut b, cs("v_str"));
        let kint = ddog_v1_intern_string(&mut b, cs("k_int"));

        let ci = ddog_v1_builder_new_chunk(&mut b, 0, 0x0123456789abcdef);
        let si = ddog_v1_chunk_new_span(&mut b, ci);
        ddog_v1_set_span_service(&mut b, ci, si, svc);
        ddog_v1_set_span_name(&mut b, ci, si, name);
        ddog_v1_set_span_resource(&mut b, ci, si, res);
        ddog_v1_set_span_id(&mut b, ci, si, 42);
        ddog_v1_set_span_start(&mut b, ci, si, 1_000);
        ddog_v1_set_span_duration(&mut b, ci, si, 500);
        ddog_v1_set_span_error(&mut b, ci, si, true);
        ddog_v1_set_span_kind(&mut b, ci, si, 2); // Server
        ddog_v1_add_span_attr_str(&mut b, ci, si, kstr, vstr);
        ddog_v1_add_span_attr_int(&mut b, ci, si, kint, 7);

        let payload = b.into_payload();
        let encoded = to_vec_from_v1(&payload);

        // trace_id big-endian 16 bytes present
        let expected_tid = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert!(encoded.windows(16).any(|w| w == expected_tid));
        // strings appear
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
    fn interning_streams_repeat_as_uint_id() {
        // "shared" used as service in two chunks must appear as raw bytes exactly once on the
        // wire; the second occurrence is a uint id emitted by the encoder's streaming table.
        let mut b = TracerPayloadV1Builder::default();
        let shared = ddog_v1_intern_string(&mut b, cs("shared"));
        let op1 = ddog_v1_intern_string(&mut b, cs("op1"));
        let op2 = ddog_v1_intern_string(&mut b, cs("op2"));
        // Interning the same string again returns the same id (value-keyed dedup).
        assert_eq!(ddog_v1_intern_string(&mut b, cs("shared")), shared);

        let c0 = ddog_v1_builder_new_chunk(&mut b, 0, 1);
        let s0 = ddog_v1_chunk_new_span(&mut b, c0);
        ddog_v1_set_span_service(&mut b, c0, s0, shared);
        ddog_v1_set_span_name(&mut b, c0, s0, op1);
        ddog_v1_set_span_id(&mut b, c0, s0, 1);

        let c1 = ddog_v1_builder_new_chunk(&mut b, 0, 2);
        let s1 = ddog_v1_chunk_new_span(&mut b, c1);
        ddog_v1_set_span_service(&mut b, c1, s1, shared);
        ddog_v1_set_span_name(&mut b, c1, s1, op2);
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
        let svc = ddog_v1_intern_string(&mut b, cs("svc"));
        let ev_name = ddog_v1_intern_string(&mut b, cs("exception"));
        let ts = ddog_v1_intern_string(&mut b, cs("dd=s:1"));
        let ak = ddog_v1_intern_string(&mut b, cs("link.attr"));
        let av = ddog_v1_intern_string(&mut b, cs("link.val"));
        let ek = ddog_v1_intern_string(&mut b, cs("ev.attr"));

        let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
        let si = ddog_v1_chunk_new_span(&mut b, ci);
        ddog_v1_set_span_service(&mut b, ci, si, svc);
        ddog_v1_set_span_id(&mut b, ci, si, 1);

        let li = ddog_v1_span_new_link(&mut b, ci, si);
        ddog_v1_set_link_trace_id(&mut b, ci, si, li, 0xaa, 0xbb);
        ddog_v1_set_link_span_id(&mut b, ci, si, li, 9);
        ddog_v1_set_link_flags(&mut b, ci, si, li, 1);
        ddog_v1_set_link_tracestate(&mut b, ci, si, li, ts);
        ddog_v1_add_link_attr_str(&mut b, ci, si, li, ak, av);

        let evi = ddog_v1_span_new_event(&mut b, ci, si);
        ddog_v1_set_event_time(&mut b, ci, si, evi, 123);
        ddog_v1_set_event_name(&mut b, ci, si, evi, ev_name);
        ddog_v1_add_event_attr_int(&mut b, ci, si, evi, ek, 5);

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
        let origin = ddog_v1_intern_string(&mut b, cs("lambda"));
        let svc = ddog_v1_intern_string(&mut b, cs("svc"));
        let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
        ddog_v1_set_chunk_sampling_priority(&mut b, ci, 2);
        ddog_v1_set_chunk_origin(&mut b, ci, origin);
        ddog_v1_set_chunk_sampling_mechanism(&mut b, ci, 4);
        ddog_v1_set_chunk_dropped_trace(&mut b, ci, true);
        let si = ddog_v1_chunk_new_span(&mut b, ci);
        ddog_v1_set_span_service(&mut b, ci, si, svc);
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
        let svc = ddog_v1_intern_string(&mut b, cs("svc"));
        let ci = ddog_v1_builder_new_chunk(&mut b, 0, 1);
        let si = ddog_v1_chunk_new_span(&mut b, ci);
        ddog_v1_set_span_service(&mut b, ci, si, svc);
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
}
