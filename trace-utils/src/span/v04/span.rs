// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
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

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str and check if the string is empty.
pub trait SpanText: Eq + Hash + Borrow<str> {}
/// Implement the SpanText trait for any type which satisfies the sub traits.
impl<T: Eq + Hash + Borrow<str>> SpanText for T {}

/// Checks if the `value` represents an empty string. Used to skip serializing empty strings
/// with serde.
fn is_empty_str<T: Borrow<str>>(value: &T) -> bool {
    value.borrow().is_empty()
}

/// The generic representation of a V04 span.
///
/// `T` is the type used to represent strings in the span, it can be either owned (e.g. BytesString)
/// or borrowed (e.g. &str). To define a generic function taking any `Span<T>` you can use the
/// [`SpanValue`] trait:
/// ```
/// use datadog_trace_utils::span::v04::{Span, SpanText};
/// fn foo<T: SpanText>(span: Span<T>) {
///     let _ = span.meta.get("foo");
/// }
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span<T>
where
    T: SpanText,
{
    pub service: T,
    pub name: T,
    pub resource: T,
    #[serde(skip_serializing_if = "is_empty_str")]
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
    T: SpanText,
{
    pub trace_id: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<T, T>,
    #[serde(skip_serializing_if = "is_empty_str")]
    pub tracestate: T,
    #[serde(skip_serializing_if = "is_default")]
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

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn skip_serializing_empty_fields_test() {
        let expected = b"\x87\xa7service\xa0\xa4name\xa0\xa8resource\xa0\xa8trace_id\x00\xa7span_id\x00\xa5start\x00\xa8duration\x00";
        let val: Span<&str> = Span::default();
        let serialized = rmp_serde::encode::to_vec_named(&val).unwrap();
        assert_eq!(expected, serialized.as_slice());
    }
}
