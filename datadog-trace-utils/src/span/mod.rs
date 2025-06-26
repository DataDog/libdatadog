// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod trace_utils;
pub mod v05;

use serde::ser::SerializeStruct;
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::str::FromStr;
use tinybytes::{Bytes, BytesString};
use v05::dict::SharedDict;

use crate::tracer_payload::TraceChunks;

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
    SpanEvents,
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
            "span_events" => Ok(SpanKey::SpanEvents),
            _ => Err(SpanKeyParseError::new(format!("Invalid span key: {s}"))),
        }
    }
}

/// Trait representing the requirements for a type to be used as a Span "string" type.
/// Note: Borrow<str> is not required by the derived traits, but allows to access HashMap elements
/// from a static str and check if the string is empty.
pub trait SpanText: Eq + Hash + Borrow<str> + Serialize + Default + Clone {
    fn from_static_str(value: &'static str) -> Self;
}

impl SpanText for &str {
    fn from_static_str(value: &'static str) -> Self {
        value
    }
}

impl SpanText for BytesString {
    fn from_static_str(value: &'static str) -> Self {
        BytesString::from_static(value)
    }
}

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
/// use datadog_trace_utils::span::{Span, SpanText};
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
    // TODO: APMSP-1941 - Replace `Bytes` with a wrapper that borrows the underlying
    // slice and serializes to bytes in MessagePack.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta_struct: HashMap<T, Bytes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLink<T>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_events: Vec<SpanEvent<T>>,
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

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SpanEvent<T>
where
    T: SpanText,
{
    pub time_unix_nano: u64,
    pub name: T,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<T, AttributeAnyValue<T>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttributeAnyValue<T>
where
    T: SpanText,
{
    SingleValue(AttributeArrayValue<T>),
    Array(Vec<AttributeArrayValue<T>>),
}

impl<T> Serialize for AttributeAnyValue<T>
where
    T: SpanText,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AttributeAnyValue", 2)?;

        match self {
            AttributeAnyValue::SingleValue(attribute) => {
                serialize_attribute_array::<S, T>(&mut state, attribute)?;
            }
            AttributeAnyValue::Array(value) => {
                let value_type: u8 = self.into();
                state.serialize_field("type", &value_type)?;
                state.serialize_field("array_value", value)?;
            }
        }

        state.end()
    }
}

impl<T> From<&AttributeAnyValue<T>> for u8
where
    T: SpanText,
{
    fn from(attribute: &AttributeAnyValue<T>) -> u8 {
        match attribute {
            AttributeAnyValue::SingleValue(value) => value.into(),
            AttributeAnyValue::Array(_) => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttributeArrayValue<T>
where
    T: SpanText,
{
    String(T),
    Boolean(bool),
    Integer(i64),
    Double(f64),
}

impl<T> Serialize for AttributeArrayValue<T>
where
    T: SpanText,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("AttributeArrayValue", 2)?;
        serialize_attribute_array::<S, T>(&mut state, self)?;
        state.end()
    }
}

fn serialize_attribute_array<S, T>(
    state: &mut S::SerializeStruct,
    attribute: &AttributeArrayValue<T>,
) -> Result<(), <S>::Error>
where
    T: SpanText,
    S: serde::Serializer,
{
    let attribute_type: u8 = attribute.into();
    state.serialize_field("type", &attribute_type)?;
    match attribute {
        AttributeArrayValue::String(value) => state.serialize_field("string_value", value),
        AttributeArrayValue::Boolean(value) => state.serialize_field("bool_value", value),
        AttributeArrayValue::Integer(value) => state.serialize_field("int_value", value),
        AttributeArrayValue::Double(value) => state.serialize_field("double_value", value),
    }
}

impl<T> From<&AttributeArrayValue<T>> for u8
where
    T: SpanText,
{
    fn from(attribute: &AttributeArrayValue<T>) -> u8 {
        match attribute {
            AttributeArrayValue::String(_) => 0,
            AttributeArrayValue::Boolean(_) => 1,
            AttributeArrayValue::Integer(_) => 2,
            AttributeArrayValue::Double(_) => 3,
        }
    }
}

pub type SpanBytes = Span<BytesString>;
pub type SpanLinkBytes = SpanLink<BytesString>;
pub type SpanEventBytes = SpanEvent<BytesString>;
pub type AttributeAnyValueBytes = AttributeAnyValue<BytesString>;
pub type AttributeArrayValueBytes = AttributeArrayValue<BytesString>;

pub type SpanSlice<'a> = Span<&'a str>;
pub type SpanLinkSlice<'a> = SpanLink<&'a str>;
pub type SpanEventSlice<'a> = SpanEvent<&'a str>;
pub type AttributeAnyValueSlice<'a> = AttributeAnyValue<&'a str>;
pub type AttributeArrayValueSlice<'a> = AttributeArrayValue<&'a str>;

pub type TraceChunksBytes = TraceChunks<BytesString>;

pub type SharedDictBytes = SharedDict<BytesString>;

impl SpanSlice<'_> {
    /// Converts a borrowed `SpanSlice` into an owned `SpanBytes`, by resolving all internal
    /// references into slices of the provided `Bytes` buffer. Returns `None` if any slice is
    /// out of bounds or invalid.
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<SpanBytes> {
        Some(SpanBytes {
            service: BytesString::try_from_bytes_slice(bytes, self.service)?,
            name: BytesString::try_from_bytes_slice(bytes, self.name)?,
            resource: BytesString::try_from_bytes_slice(bytes, self.resource)?,
            r#type: BytesString::try_from_bytes_slice(bytes, self.r#type)?,
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_id: self.parent_id,
            start: self.start,
            duration: self.duration,
            error: self.error,
            meta: self
                .meta
                .iter()
                .map(|(k, v)| {
                    Some((
                        BytesString::try_from_bytes_slice(bytes, k)?,
                        BytesString::try_from_bytes_slice(bytes, v)?,
                    ))
                })
                .collect::<Option<HashMap<BytesString, BytesString>>>()?,
            metrics: self
                .metrics
                .iter()
                .map(|(k, v)| Some((BytesString::try_from_bytes_slice(bytes, k)?, *v)))
                .collect::<Option<HashMap<BytesString, f64>>>()?,
            meta_struct: self
                .meta_struct
                .iter()
                .map(|(k, v)| Some((BytesString::try_from_bytes_slice(bytes, k)?, v.clone())))
                .collect::<Option<HashMap<BytesString, Bytes>>>()?,
            span_links: self
                .span_links
                .iter()
                .map(|link| link.try_to_bytes(bytes))
                .collect::<Option<Vec<SpanLinkBytes>>>()?,
            span_events: self
                .span_events
                .iter()
                .map(|event| event.try_to_bytes(bytes))
                .collect::<Option<Vec<SpanEventBytes>>>()?,
        })
    }
}

impl SpanLinkSlice<'_> {
    /// Converts a borrowed `SpanLinkSlice` into an owned `SpanLinkBytes`, using the provided
    /// `Bytes` buffer to resolve all referenced strings. Returns `None` if conversion fails due
    /// to invalid slice ranges.
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<SpanLinkBytes> {
        Some(SpanLinkBytes {
            trace_id: self.trace_id,
            trace_id_high: self.trace_id_high,
            span_id: self.span_id,
            attributes: self
                .attributes
                .iter()
                .map(|(k, v)| {
                    Some((
                        BytesString::try_from_bytes_slice(bytes, k)?,
                        BytesString::try_from_bytes_slice(bytes, v)?,
                    ))
                })
                .collect::<Option<HashMap<BytesString, BytesString>>>()?,
            tracestate: BytesString::try_from_bytes_slice(bytes, self.tracestate)?,
            flags: self.flags,
        })
    }
}

impl SpanEventSlice<'_> {
    /// Converts a borrowed `SpanEventSlice` into an owned `SpanEventBytes`, resolving references
    /// into the provided `Bytes` buffer. Fails with `None` if any slice is invalid or cannot be
    /// converted.
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<SpanEventBytes> {
        Some(SpanEventBytes {
            time_unix_nano: self.time_unix_nano,
            name: BytesString::try_from_bytes_slice(bytes, self.name)?,
            attributes: self
                .attributes
                .iter()
                .map(|(k, v)| {
                    Some((
                        BytesString::try_from_bytes_slice(bytes, k)?,
                        v.try_to_bytes(bytes)?,
                    ))
                })
                .collect::<Option<HashMap<BytesString, AttributeAnyValueBytes>>>()?,
        })
    }
}

impl AttributeAnyValueSlice<'_> {
    /// Converts a borrowed `AttributeAnyValueSlice` into its owned `AttributeAnyValueBytes`
    /// representation, using the provided `Bytes` buffer. Recursively processes inner values if
    /// it's an array.
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<AttributeAnyValueBytes> {
        match self {
            AttributeAnyValue::SingleValue(value) => {
                Some(AttributeAnyValue::SingleValue(value.try_to_bytes(bytes)?))
            }
            AttributeAnyValue::Array(value) => Some(AttributeAnyValue::Array(
                value
                    .iter()
                    .map(|attribute| attribute.try_to_bytes(bytes))
                    .collect::<Option<Vec<AttributeArrayValueBytes>>>()?,
            )),
        }
    }
}

impl AttributeArrayValueSlice<'_> {
    /// Converts a single `AttributeArrayValueSlice` item into its owned form
    /// (`AttributeArrayValueBytes`), borrowing data from the provided `Bytes` buffer when
    /// necessary.
    pub fn try_to_bytes(&self, bytes: &Bytes) -> Option<AttributeArrayValueBytes> {
        match self {
            AttributeArrayValue::String(value) => Some(AttributeArrayValue::String(
                BytesString::try_from_bytes_slice(bytes, value)?,
            )),
            AttributeArrayValue::Boolean(value) => Some(AttributeArrayValue::Boolean(*value)),
            AttributeArrayValue::Integer(value) => Some(AttributeArrayValue::Integer(*value)),
            AttributeArrayValue::Double(value) => Some(AttributeArrayValue::Double(*value)),
        }
    }
}

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
    use super::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
    use crate::msgpack_decoder::v04::span::decode_span;
    use std::collections::HashMap;

    #[test]
    fn skip_serializing_empty_fields_test() {
        let expected = b"\x87\xa7service\xa0\xa4name\xa0\xa8resource\xa0\xa8trace_id\x00\xa7span_id\x00\xa5start\x00\xa8duration\x00";
        let val: Span<&str> = Span::default();
        let serialized = rmp_serde::encode::to_vec_named(&val).unwrap();
        assert_eq!(expected, serialized.as_slice());
    }

    #[test]
    fn serialize_deserialize_test() {
        let span: Span<&str> = Span {
            name: "tracing.operation",
            resource: "MyEndpoint",
            span_links: vec![SpanLink {
                trace_id: 42,
                attributes: HashMap::from([("span", "link")]),
                tracestate: "running",
                ..Default::default()
            }],
            span_events: vec![SpanEvent {
                time_unix_nano: 1727211691770716000,
                name: "exception",
                attributes: HashMap::from([
                    (
                        "exception.message",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::String(
                            "Cannot divide by zero",
                        )),
                    ),
                    (
                        "exception.type",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::String("RuntimeError")),
                    ),
                    (
                        "exception.escaped",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Boolean(false)),
                    ),
                    (
                        "exception.count",
                        AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(1)),
                    ),
                    (
                        "exception.lines",
                        AttributeAnyValue::Array(vec![
                            AttributeArrayValue::String("  File \"<string>\", line 1, in <module>"),
                            AttributeArrayValue::String("  File \"<string>\", line 1, in divide"),
                            AttributeArrayValue::String("RuntimeError: Cannot divide by zero"),
                        ]),
                    ),
                ]),
            }],
            ..Default::default()
        };

        let serialized = rmp_serde::encode::to_vec_named(&span).unwrap();
        let mut serialized_slice = serialized.as_ref();
        let deserialized = decode_span(&mut serialized_slice).unwrap();

        assert_eq!(span.name, deserialized.name);
        assert_eq!(span.resource, deserialized.resource);
        assert_eq!(
            span.span_links[0].trace_id,
            deserialized.span_links[0].trace_id
        );
        assert_eq!(
            span.span_links[0].tracestate,
            deserialized.span_links[0].tracestate
        );
        assert_eq!(span.span_events[0].name, deserialized.span_events[0].name);
        assert_eq!(
            span.span_events[0].time_unix_nano,
            deserialized.span_events[0].time_unix_nano
        );
        for attribut in &deserialized.span_events[0].attributes {
            assert!(span.span_events[0].attributes.contains_key(attribut.0))
        }
    }

    #[test]
    fn serialize_event_test() {
        // `expected` is created by transforming the span into bytes
        // and passing each bytes through `escaped_default`
        let expected = b"\x88\xa7service\xa0\xa4name\xa0\xa8resource\xa0\xa8trace_id\x00\xa7span_id\x00\xa5start\x00\xa8duration\x00\xabspan_events\x91\x83\xaetime_unix_nano\xcf\x17\xf8I\xe1\xeb\xe5\x1f`\xa4name\xa4test\xaaattributes\x81\xaatest.event\x82\xa4type\x03\xacdouble_value\xcb@\x10\xcc\xcc\xcc\xcc\xcc\xcd";

        let span: Span<&str> = Span {
            span_events: vec![SpanEvent {
                time_unix_nano: 1727211691770716000,
                name: "test",
                attributes: HashMap::from([(
                    "test.event",
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Double(4.2)),
                )]),
            }],
            ..Default::default()
        };

        let serialized = rmp_serde::encode::to_vec_named(&span).unwrap();
        assert_eq!(expected, serialized.as_slice());
    }
}
