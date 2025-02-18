// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use tinybytes::BytesString;

#[derive(Debug, PartialEq)]
pub enum SpanKey {
    Service,
    Name,
    Resource,
    TraceId,
    SpanId,
    ParentId,
    Start,
    Duration,
    Error,
    Meta,
    Metrics,
    Type,
    MetaStruct,
    SpanLinks,
}

impl FromStr for SpanKey {
    type Err = SpanKeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "service" => Ok(SpanKey::Service),
            "name" => Ok(SpanKey::Name),
            "resource" => Ok(SpanKey::Resource),
            "trace_id" => Ok(SpanKey::TraceId),
            "span_id" => Ok(SpanKey::SpanId),
            "parent_id" => Ok(SpanKey::ParentId),
            "start" => Ok(SpanKey::Start),
            "duration" => Ok(SpanKey::Duration),
            "error" => Ok(SpanKey::Error),
            "meta" => Ok(SpanKey::Meta),
            "metrics" => Ok(SpanKey::Metrics),
            "type" => Ok(SpanKey::Type),
            "meta_struct" => Ok(SpanKey::MetaStruct),
            "span_links" => Ok(SpanKey::SpanLinks),
            _ => Err(SpanKeyParseError::new(format!("Invalid span key: {}", s))),
        }
    }
}

use std::borrow::Borrow;
use std::hash::Hash;

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str.
pub trait SpanValue: Eq + Hash + Borrow<str> {}
/// Implement the SpanValue trait for any type which satisfies the sub trait.
impl<T: Eq + Hash + Borrow<str>> SpanValue for T {}

/// The generic representation of a V04 span.
///
/// `T` is the type used to represent strings in the span, it can be either owned (e.g. BytesString)
/// or borrowed (e.g. &str). To define a generic function taking any `Span<T>` you can use the
/// [`SpanValue`] trait:
/// ```
/// use datadog_trace_utils::span::v04::{Span, SpanValue};
/// fn foo<T: SpanValue>(span: Span<T>) {
///     let _ = span.meta.get("foo");
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span<T>
where
    T: SpanValue,
{
    pub service: T,
    pub name: T,
    pub resource: T,
    pub r#type: T,
    pub trace_id: u64,
    pub span_id: u64,
    #[serde(skip_serializing_if = "is_default")]
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    #[serde(skip_serializing_if = "is_default")]
    pub error: i32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<T, T>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub metrics: HashMap<T, f64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta_struct: HashMap<T, Vec<u8>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLink<T>>,
}

/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpanLink<T>
where
    T: SpanValue,
{
    pub trace_id: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    pub attributes: HashMap<T, T>,
    pub tracestate: T,
    pub flags: u64,
}

pub type SpanBytes = Span<BytesString>;
pub type SpanLinkBytes = SpanLink<BytesString>;

#[derive(Debug)]
pub struct SpanKeyParseError {
    pub message: String,
}

impl SpanKeyParseError {
    pub fn new(message: impl Into<String>) -> Self {
        SpanKeyParseError {
            message: message.into(),
        }
    }
}
impl fmt::Display for SpanKeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SpanKeyParseError: {}", self.message)
    }
}
impl std::error::Error for SpanKeyParseError {}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}
