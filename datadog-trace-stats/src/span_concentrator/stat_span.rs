// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implements a common interface for spans used to compute stats. It is used to
//! support both trace-utils' Span and pb::Span.

use datadog_trace_protobuf::pb;
use datadog_trace_utils::span::{trace_utils, Span, SpanText};
use datadog_trace_utils::trace_utils as pb_utils;

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

impl<'a, T: SpanText> StatSpan<'a> for Span<T> {
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
        pb_utils::is_measured(self)
    }

    fn is_partial_snapshot(&'a self) -> bool {
        pb_utils::is_partial_snapshot(self)
    }

    fn has_top_level(&'a self) -> bool {
        pb_utils::has_top_level(self)
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        self.meta.get(key).map(|v| v.as_str())
    }

    fn get_metrics(&'a self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }
}
