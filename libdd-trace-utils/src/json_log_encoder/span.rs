// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serialization of a single [`Span`] into the Datadog Forwarder "log exporter"
//! JSON shape (APM v0.2 span schema).
//!
//! This deliberately does **not** reuse the derived [`serde::Serialize`] impl on
//! [`Span`], which targets the msgpack v04 wire format (numeric ids, different
//! skip rules). The log format requires `trace_id`/`span_id`/`parent_id` as
//! zero-padded lowercase hex strings, `error` always present, and `type`/`meta`/
//! `metrics`/`meta_struct`/`span_links`/`span_events` omitted when empty.
//!
//! The wrappers below serialize via the span's associated text type
//! (`T::Text`, which is always [`serde::Serialize`] through the `SpanText`
//! trait bound) rather than the whole `T`, so the encoder does not require a
//! `T: Serialize` bound — keeping the public exporter API free of that bound.

use crate::span::v04::{Span, SpanEvent, SpanLink};
use crate::span::TraceData;
use serde::ser::{SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};
use std::borrow::Borrow;

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Writes `value` as 16 zero-padded lowercase hex bytes into the start of `out`.
///
/// The caller guarantees `out` has room for at least 16 bytes.
fn fill_hex(out: &mut [u8], value: u64) {
    for i in 0..16 {
        out[15 - i] = HEX_DIGITS[((value >> (i * 4)) & 0xf) as usize];
    }
}

/// Serializes `buf` as a JSON string. `buf` must only contain ASCII hex digits,
/// so the UTF-8 check below never fails in practice.
fn serialize_ascii_hex<S: Serializer>(serializer: S, buf: &[u8]) -> Result<S::Ok, S::Error> {
    match std::str::from_utf8(buf) {
        Ok(text) => serializer.serialize_str(text),
        // Unreachable: `fill_hex` only writes ASCII hex digits.
        Err(_) => serializer.serialize_str(""),
    }
}

/// A `u64` id rendered as a 16-char zero-padded lowercase hex JSON string.
struct HexU64(u64);

impl Serialize for HexU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut buf = [0u8; 16];
        fill_hex(&mut buf, self.0);
        serialize_ascii_hex(serializer, &buf)
    }
}

/// A 128-bit trace id rendered as hex. When the high 64 bits are zero it is
/// emitted as 16 hex chars (the low 64 bits); otherwise as the full 32 chars.
struct HexTraceId(u128);

impl Serialize for HexTraceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let high = (self.0 >> 64) as u64;
        let low = self.0 as u64;
        if high == 0 {
            let mut buf = [0u8; 16];
            fill_hex(&mut buf, low);
            serialize_ascii_hex(serializer, &buf)
        } else {
            let mut buf = [0u8; 32];
            fill_hex(&mut buf[0..16], high);
            fill_hex(&mut buf[16..32], low);
            serialize_ascii_hex(serializer, &buf)
        }
    }
}

/// Serializes a [`SpanLink`] in the v04 JSON shape without requiring `T:
/// Serialize` (only `T::Text`, via the `SpanText` bound, is needed).
struct LogSpanLink<'a, T: TraceData>(&'a SpanLink<T>);

impl<T: TraceData> Serialize for LogSpanLink<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let link = self.0;
        let has_attributes = !link.attributes.is_empty();
        let has_tracestate = !Borrow::<str>::borrow(&link.tracestate).is_empty();
        let has_flags = link.flags != 0;

        // Always: trace_id, trace_id_high, span_id.
        let mut len = 3;
        len += has_attributes as usize;
        len += has_tracestate as usize;
        len += has_flags as usize;

        let mut state = serializer.serialize_struct("span_link", len)?;
        state.serialize_field("trace_id", &link.trace_id)?;
        state.serialize_field("trace_id_high", &link.trace_id_high)?;
        state.serialize_field("span_id", &link.span_id)?;
        if has_attributes {
            state.serialize_field("attributes", &link.attributes)?;
        }
        if has_tracestate {
            state.serialize_field("tracestate", &link.tracestate)?;
        }
        if has_flags {
            state.serialize_field("flags", &link.flags)?;
        }
        state.end()
    }
}

/// Serializes a [`SpanEvent`] in the v04 JSON shape without requiring `T:
/// Serialize`. The event's attribute values (`AttributeAnyValue<T>`) already
/// serialize via a `T::Text`-bounded impl.
struct LogSpanEvent<'a, T: TraceData>(&'a SpanEvent<T>);

impl<T: TraceData> Serialize for LogSpanEvent<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let event = self.0;
        let has_attributes = !event.attributes.is_empty();

        // Always: time_unix_nano, name.
        let mut len = 2;
        len += has_attributes as usize;

        let mut state = serializer.serialize_struct("span_event", len)?;
        state.serialize_field("time_unix_nano", &event.time_unix_nano)?;
        state.serialize_field("name", &event.name)?;
        if has_attributes {
            state.serialize_field("attributes", &event.attributes)?;
        }
        state.end()
    }
}

/// Serializes a slice of `Item` as a JSON array using the `Wrap` per-element
/// wrapper, avoiding a `T: Serialize` bound on the element type.
struct LogSeq<'a, I, W>(&'a [I], fn(&'a I) -> W);

impl<I, W: Serialize> Serialize for LogSeq<'_, I, W> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            seq.serialize_element(&(self.1)(item))?;
        }
        seq.end()
    }
}

/// Wraps a [`Span`] so that [`serde_json`] emits the Datadog Forwarder log shape.
pub(crate) struct LogSpan<'a, T: TraceData>(pub &'a Span<T>);

impl<T: TraceData> Serialize for LogSpan<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let span = self.0;

        let has_type = !Borrow::<str>::borrow(&span.r#type).is_empty();
        let has_meta = !span.meta.is_empty();
        let has_metrics = !span.metrics.is_empty();
        let has_links = !span.span_links.is_empty();
        let has_events = !span.span_events.is_empty();

        // Required: trace_id, span_id, parent_id, service, name, resource, error,
        // start, duration. `meta_struct` is intentionally NOT emitted: it holds raw
        // msgpack bytes that would serialize as a JSON number array the intake
        // cannot interpret (the reference JS/Go/Py/Java exporters omit it too).
        let mut len = 9;
        len += has_type as usize;
        len += has_meta as usize;
        len += has_metrics as usize;
        len += has_links as usize;
        len += has_events as usize;

        let mut state = serializer.serialize_struct("span", len)?;
        state.serialize_field("trace_id", &HexTraceId(span.trace_id))?;
        state.serialize_field("span_id", &HexU64(span.span_id))?;
        state.serialize_field("parent_id", &HexU64(span.parent_id))?;
        state.serialize_field("service", &span.service)?;
        state.serialize_field("name", &span.name)?;
        state.serialize_field("resource", &span.resource)?;
        if has_type {
            state.serialize_field("type", &span.r#type)?;
        }
        state.serialize_field("error", &span.error)?;
        state.serialize_field("start", &span.start)?;
        state.serialize_field("duration", &span.duration)?;
        if has_meta {
            state.serialize_field("meta", &span.meta)?;
        }
        if has_metrics {
            state.serialize_field("metrics", &span.metrics)?;
        }
        if has_links {
            state.serialize_field("span_links", &LogSeq(&span.span_links, LogSpanLink))?;
        }
        if has_events {
            state.serialize_field("span_events", &LogSeq(&span.span_events, LogSpanEvent))?;
        }
        state.end()
    }
}
