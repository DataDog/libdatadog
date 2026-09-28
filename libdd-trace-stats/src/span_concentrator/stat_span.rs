// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implements a common interface for spans used to compute stats. It is used to
//! support both trace-utils' Span and pb::Span.

use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::v1::{AttributeValue, Span as SpanV1, SpanKind, TraceChunk};
// trace_utils (v04), trace_utils_v1, and trace_utils_pb (aliased below) all expose functions
// with the same names (is_measured, is_partial_snapshot, has_top_level), one per span
// representation.
use libdd_trace_utils::span::{trace_utils, trace_utils_v1, v04::Span, TraceData};
use libdd_trace_utils::trace_utils as trace_utils_pb;
use std::borrow::Borrow;

/// Common interface for spans used in stats computation
pub trait StatSpan<'a> {
    /// Returns the service name
    fn service(&'a self) -> &'a str;
    /// Returns the resource name
    fn resource(&'a self) -> &'a str;
    /// Returns the operation name
    fn name(&'a self) -> &'a str;
    /// Returns the span type
    fn r#type(&'a self) -> &'a str;
    /// Returns the start timestamp
    fn start(&'a self) -> i64;
    /// Returns the duration
    fn duration(&'a self) -> i64;
    /// Returns true if the span is an error
    fn is_error(&'a self) -> bool;
    /// Returns true if the span is a trace root
    fn is_trace_root(&'a self) -> bool;
    /// Returns true if the span is measured
    fn is_measured(&'a self) -> bool;
    /// Returns true if the span is a partial snapshot
    fn is_partial_snapshot(&'a self) -> bool;
    /// Returns true if the span has a top level key set
    fn has_top_level(&'a self) -> bool;
    /// Returns the value of a meta field
    fn get_meta(&'a self, key: &str) -> Option<&'a str>;
    /// Returns the value of a metrics field
    fn get_metrics(&'a self, key: &str) -> Option<f64>;
}

impl<'a, T: TraceData> StatSpan<'a> for Span<T> {
    fn service(&'a self) -> &'a str {
        self.service.borrow()
    }

    fn resource(&'a self) -> &'a str {
        self.resource.borrow()
    }

    fn name(&'a self) -> &'a str {
        self.name.borrow()
    }

    fn r#type(&'a self) -> &'a str {
        self.r#type.borrow()
    }

    fn start(&'a self) -> i64 {
        self.start
    }

    fn duration(&'a self) -> i64 {
        self.duration
    }

    fn is_error(&'a self) -> bool {
        self.error != 0
    }

    fn is_trace_root(&'a self) -> bool {
        self.parent_id == 0
    }

    fn is_measured(&'a self) -> bool {
        trace_utils::is_measured(self)
    }

    fn is_partial_snapshot(&'a self) -> bool {
        trace_utils::is_partial_snapshot(self)
    }

    fn has_top_level(&'a self) -> bool {
        trace_utils::has_top_level(self)
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        self.meta.get(key).map(|v| v.borrow())
    }

    fn get_metrics(&'a self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }
}

impl<'a, T: TraceData> StatSpan<'a> for SpanV1<T> {
    fn service(&'a self) -> &'a str {
        self.service.borrow()
    }

    fn resource(&'a self) -> &'a str {
        self.resource.borrow()
    }

    fn name(&'a self) -> &'a str {
        self.name.borrow()
    }

    fn r#type(&'a self) -> &'a str {
        self.r#type.borrow()
    }

    fn start(&'a self) -> i64 {
        self.start
    }

    fn duration(&'a self) -> i64 {
        self.duration
    }

    fn is_error(&'a self) -> bool {
        self.error
    }

    fn is_trace_root(&'a self) -> bool {
        self.parent_id == 0
    }

    fn is_measured(&'a self) -> bool {
        trace_utils_v1::is_measured(self)
    }

    fn is_partial_snapshot(&'a self) -> bool {
        trace_utils_v1::is_partial_snapshot(self)
    }

    fn has_top_level(&'a self) -> bool {
        trace_utils_v1::has_top_level(self)
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        match self.attributes.get(key) {
            Some(AttributeValue::String(s)) => Some(s.borrow()),
            // `span_kind` is a dedicated field rather than an attribute; expose it under the same
            // "span.kind" key that v0.4 spans (and is_span_eligible) look for. `Internal` is the
            // wire-level default and indistinguishable from "unset", so it's treated as no value
            // here, leaving room for a chunk-level fallback (see `ChunkSpanView`).
            _ if key == "span.kind" && self.span_kind != SpanKind::Internal => {
                Some(self.span_kind.as_meta_str())
            }
            _ => None,
        }
    }

    fn get_metrics(&'a self, key: &str) -> Option<f64> {
        match self.attributes.get(key) {
            Some(AttributeValue::Float(v)) => Some(*v),
            Some(AttributeValue::Int(v)) => Some(*v as f64),
            _ => None,
        }
    }
}

/// Wraps a V1 span together with its enclosing chunk.
///
/// `TraceChunk.attributes` holds values common to every span in the chunk (e.g. peer tags), and
/// `TraceChunk.origin` holds the `_dd.origin` value for the whole chunk. `get_meta`/`get_metrics`
/// fall back to these chunk-level values whenever the span doesn't have its own value for a
/// given key.
pub struct ChunkSpanView<'a, T: TraceData> {
    pub span: &'a SpanV1<T>,
    pub chunk: &'a TraceChunk<T>,
}

impl<'a, T: TraceData> StatSpan<'a> for ChunkSpanView<'a, T> {
    fn service(&'a self) -> &'a str {
        self.span.service()
    }

    fn resource(&'a self) -> &'a str {
        self.span.resource()
    }

    fn name(&'a self) -> &'a str {
        self.span.name()
    }

    fn r#type(&'a self) -> &'a str {
        self.span.r#type()
    }

    fn start(&'a self) -> i64 {
        self.span.start()
    }

    fn duration(&'a self) -> i64 {
        self.span.duration()
    }

    fn is_error(&'a self) -> bool {
        self.span.is_error()
    }

    fn is_trace_root(&'a self) -> bool {
        self.span.is_trace_root()
    }

    fn is_measured(&'a self) -> bool {
        self.span.is_measured()
    }

    fn is_partial_snapshot(&'a self) -> bool {
        self.span.is_partial_snapshot()
    }

    fn has_top_level(&'a self) -> bool {
        self.span.has_top_level()
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        self.span.get_meta(key).or_else(|| {
            // `origin` is a dedicated chunk field rather than a chunk attribute; expose it
            // under the same key aggregation.rs's TAG_ORIGIN looks for.
            if key == "_dd.origin" {
                let origin = self.chunk.origin.borrow();
                if !origin.is_empty() {
                    return Some(origin);
                }
            }
            match self.chunk.attributes.get(key) {
                Some(AttributeValue::String(s)) => Some(s.borrow()),
                _ => None,
            }
        })
    }

    fn get_metrics(&'a self, key: &str) -> Option<f64> {
        self.span
            .get_metrics(key)
            .or_else(|| match self.chunk.attributes.get(key) {
                Some(AttributeValue::Float(v)) => Some(*v),
                Some(AttributeValue::Int(v)) => Some(*v as f64),
                _ => None,
            })
    }
}

impl<'a> StatSpan<'a> for pb::Span {
    fn service(&'a self) -> &'a str {
        self.service.as_str()
    }

    fn resource(&'a self) -> &'a str {
        self.resource.as_str()
    }

    fn name(&'a self) -> &'a str {
        self.name.as_str()
    }

    fn r#type(&'a self) -> &'a str {
        self.r#type.as_str()
    }

    fn start(&'a self) -> i64 {
        self.start
    }

    fn duration(&'a self) -> i64 {
        self.duration
    }

    fn is_error(&'a self) -> bool {
        self.error != 0
    }

    fn is_trace_root(&'a self) -> bool {
        self.parent_id == 0
    }

    fn is_measured(&'a self) -> bool {
        trace_utils_pb::is_measured(self)
    }

    fn is_partial_snapshot(&'a self) -> bool {
        trace_utils_pb::is_partial_snapshot(self)
    }

    fn has_top_level(&'a self) -> bool {
        trace_utils_pb::has_top_level(self)
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        self.meta.get(key).map(|v| v.as_str())
    }

    fn get_metrics(&'a self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }
}
