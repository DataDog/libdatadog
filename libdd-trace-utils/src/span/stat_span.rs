// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implements a common interface for spans used to compute stats. It is used to
//! support both trace-utils' Span and pb::Span.

use super::{trace_utils, v04::Span, TraceData};
use crate::trace_utils as pb_utils;
use libdd_trace_protobuf::pb;
use std::borrow::Borrow;

/// Common interface for spans used in stats computation
pub trait StatSpan {
    /// Returns the service name
    fn service(&self) -> &str;
    /// Returns the resource name
    fn resource(&self) -> &str;
    /// Returns the operation name
    fn name(&self) -> &str;
    /// Returns the span type
    fn r#type(&self) -> &str;
    /// Returns the start timestamp
    fn start(&self) -> i64;
    /// Returns the duration
    fn duration(&self) -> i64;
    /// Returns true if the span is an error
    fn is_error(&self) -> bool;
    /// Returns true if the span is a trace root
    fn is_trace_root(&self) -> bool;
    /// Returns true if the span is measured
    fn is_measured(&self) -> bool;
    /// Returns true if the span is a partial snapshot
    fn is_partial_snapshot(&self) -> bool;
    /// Returns true if the span has a top level key set
    fn has_top_level(&self) -> bool;
    /// Returns the value of a meta field
    fn get_meta(&self, key: &str) -> Option<&str>;
    /// Returns the value of a metrics field
    fn get_metrics(&self, key: &str) -> Option<f64>;
    /// Returns the trace id
    fn trace_id(&self) -> u128;
    /// Returns the span id
    fn span_id(&self) -> u64;
}

// Common interface for generic span transforming
pub(crate) trait StatSpanMut {
    fn set_service(&mut self, service: String);

    fn start_mut(&mut self) -> &mut i64;
    fn duration_mut(&mut self) -> &mut i64;
    fn span_id_mut(&mut self) -> &mut u64;
}

impl<T: TraceData> StatSpan for Span<T> {
    fn service(&self) -> &str {
        self.service.borrow()
    }

    fn resource(&self) -> &str {
        self.resource.borrow()
    }

    fn name(&self) -> &str {
        self.name.borrow()
    }

    fn r#type(&self) -> &str {
        self.r#type.borrow()
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn is_error(&self) -> bool {
        self.error != 0
    }

    fn is_trace_root(&self) -> bool {
        self.parent_id == 0
    }

    fn is_measured(&self) -> bool {
        trace_utils::is_measured(self)
    }

    fn is_partial_snapshot(&self) -> bool {
        trace_utils::is_partial_snapshot(self)
    }

    fn has_top_level(&self) -> bool {
        trace_utils::has_top_level(self)
    }

    fn get_meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(|v| v.borrow())
    }

    fn get_metrics(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }

    fn trace_id(&self) -> u128 {
        self.trace_id
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }
}

impl StatSpan for pb::Span {
    fn service(&self) -> &str {
        self.service.as_str()
    }

    fn resource(&self) -> &str {
        self.resource.as_str()
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn r#type(&self) -> &str {
        self.r#type.as_str()
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn is_error(&self) -> bool {
        self.error != 0
    }

    fn is_trace_root(&self) -> bool {
        self.parent_id == 0
    }

    fn is_measured(&self) -> bool {
        pb_utils::is_measured(self)
    }

    fn is_partial_snapshot(&self) -> bool {
        pb_utils::is_partial_snapshot(self)
    }

    fn has_top_level(&self) -> bool {
        pb_utils::has_top_level(self)
    }

    fn get_meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(|v| v.as_str())
    }

    fn get_metrics(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }

    fn trace_id(&self) -> u128 {
        self.trace_id.into()
    }
    fn span_id(&self) -> u64 {
        self.span_id
    }
}

impl<T: TraceData> StatSpanMut for Span<T> {
    fn set_service(&mut self, service: String) {
        self.service = T::Text::from(service);
    }

    fn start_mut(&mut self) -> &mut i64 {
        &mut self.start
    }

    fn duration_mut(&mut self) -> &mut i64 {
        &mut self.duration
    }

    fn span_id_mut(&mut self) -> &mut u64 {
        &mut self.span_id
    }
}

impl StatSpanMut for pb::Span {
    fn set_service(&mut self, service: String) {
        self.service = service;
    }

    fn start_mut(&mut self) -> &mut i64 {
        &mut self.start
    }

    fn duration_mut(&mut self) -> &mut i64 {
        &mut self.duration
    }

    fn span_id_mut(&mut self) -> &mut u64 {
        &mut self.span_id
    }
}
