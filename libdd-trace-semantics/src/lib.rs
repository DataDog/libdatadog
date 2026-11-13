// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The Datadog trace semantics registry: which span attributes hold the same concept across
//! Datadog and OpenTelemetry conventions, and in which order to read them.
//!
//! The registry is the one the Trace Agent uses (`pkg/trace/semantics`). It is owned by Semantic
//! Core and compiled into the library from `mappings.json`, so lookups never parse anything at
//! runtime and work without an Agent.
//!
//! ```
//! use libdd_trace_semantics::{entries, lookup_str, Concept};
//! # use libdd_trace_model::Attributes;
//! # fn method(span: &impl Attributes) -> Option<std::borrow::Cow<'_, str>> {
//! // The first non-empty value among `http.request.method` and `http.method`.
//! lookup_str(span, Concept::HttpMethod)
//! # }
//! assert_eq!(entries(Concept::HttpMethod)[0].name, "http.request.method");
//! ```
//!
//! The library only says where a concept is stored. It never renames attributes. Keys chosen by
//! users or operators (filters, additional metric tags, peer tag lists) must stay exact and be
//! read by name, not through the registry.
//!
//! Promoted tags (e.g. `span.kind` or `_dd.origin`) live in dedicated fields, so these lookups
//! can't read them. Use the `Span` and `TraceChunk` getters instead.

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

extern crate alloc;

mod lookup;

pub use lookup::{lookup_f64, lookup_i64, lookup_str};

include!(concat!(env!("OUT_DIR"), "/registry.rs"));

/// Who defines an attribute name.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    Datadog,
    /// OpenTelemetry semantic conventions
    Otel,
}

/// The type an attribute is expected to hold.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    String,
    Int64,
    Float64,
}

/// A predicate on another attribute that must hold before an [`Entry`] is read.
///
/// Conditions read the string value of the attribute by its exact name, and treat an empty string
/// as absent, like the Agent.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Condition {
    /// The attribute the condition reads
    pub attribute: &'static str,
    /// If set, the attribute must be present (`true`) or absent (`false`)
    pub present: Option<bool>,
    /// If set, the attribute must equal this value
    pub eq: Option<&'static str>,
}

/// An attribute that can hold a [`Concept`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    /// The attribute name, e.g. `http.request.method`
    pub name: &'static str,
    pub provider: Provider,
    /// The semantic convention version that introduced the name, if any
    pub version: Option<&'static str>,
    pub value_type: ValueType,
    /// Conditions that must all hold for this entry to be read
    pub when: &'static [Condition],
}

/// The attributes that can hold `concept`, in precedence order.
#[must_use]
pub const fn entries(concept: Concept) -> &'static [Entry] {
    concept.entries()
}

/// The concept with this canonical name, e.g. `http.method`. Attribute names that are not a
/// canonical name, like `http.request.method`, return `None`.
#[must_use]
pub fn concept(name: &str) -> Option<Concept> {
    concept_by_name(name)
}

/// Every concept in the registry.
#[must_use]
pub const fn concepts() -> &'static [Concept] {
    &CONCEPTS
}

/// The registry version, as published by Semantic Core.
#[must_use]
pub const fn version() -> &'static str {
    VERSION
}

/// The registry content hash, as published by Semantic Core. It is a label copied from
/// `mappings.json`, not recomputed.
#[must_use]
pub const fn content_hash() -> &'static str {
    CONTENT_HASH
}
