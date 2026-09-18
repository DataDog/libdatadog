// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Span data structure decoupled from any specific protobuf representation.

use std::collections::HashMap;

/// Data for an inferred span, decoupled from `pb::Span`.
///
/// Consumers convert this into their own span representation (e.g.,
/// `libdd_trace_protobuf::pb::Span` or a language-specific trace span).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanData {
    /// Span operation name (e.g., "aws.httpapi", "aws.sqs").
    pub name: String,
    /// Service name, resolved via service mapping.
    pub service: String,
    /// Resource name (e.g., "GET /users/{id}", queue name).
    pub resource: String,
    /// Span type (e.g., "web").
    pub r#type: String,
    /// Start time in nanoseconds since Unix epoch.
    pub start: i64,
    /// Key-value metadata for the span.
    pub meta: HashMap<String, String>,
}
