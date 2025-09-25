// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{BytesData, SliceData, SpanKeyParseError, TraceData};
use crate::tracer_payload::TraceChunks;
use serde::ser::SerializeStruct;
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::str::FromStr;

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
/// use libdd_trace_utils::span::{v04::Span, TraceData};
/// fn foo<T: TraceData>(span: Span<T>) {
///     let _ = span.meta.get("foo");
/// }
/// ```
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Span<T: TraceData> {
    pub service: T::Text,
    pub name: T::Text,
    pub resource: T::Text,
    #[serde(skip_serializing_if = "is_empty_str")]
    pub r#type: T::Text,
    #[serde(serialize_with = "serialize_lower_64_bits")]
    pub trace_id: u128,
    pub span_id: u64,
    #[serde(skip_serializing_if = "is_default")]
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    #[serde(skip_serializing_if = "is_default")]
    pub error: i32,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta: HashMap<T::Text, T::Text>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub metrics: HashMap<T::Text, f64>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub meta_struct: HashMap<T::Text, T::Bytes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLink<T>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_events: Vec<SpanEvent<T>>,
}

impl<T: TraceData> Clone for Span<T>
where
    T::Text: Clone,
    T::Bytes: Clone,
{
    fn clone(&self) -> Self {
        Span {
            service: self.service.clone(),
            name: self.name.clone(),
            resource: self.resource.clone(),
            r#type: self.r#type.clone(),
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_id: self.parent_id,
            start: self.start,
            duration: self.duration,
            error: self.error,
            meta: self.meta.clone(),
            metrics: self.metrics.clone(),
            meta_struct: self.meta_struct.clone(),
            span_links: self.span_links.clone(),
            span_events: self.span_events.clone(),
        }
    }
}

fn serialize_lower_64_bits<S>(v: &u128, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u64(*v as u64)
}

/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct SpanLink<T: TraceData> {
    pub trace_id: u64,
    pub trace_id_high: u64,
    pub span_id: u64,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<T::Text, T::Text>,
    #[serde(skip_serializing_if = "is_empty_str")]
    pub tracestate: T::Text,
    #[serde(skip_serializing_if = "is_default")]
    pub flags: u32,
}

impl<T: TraceData> Clone for SpanLink<T>
where
    T::Text: Clone,
{
    fn clone(&self) -> Self {
        SpanLink {
            trace_id: self.trace_id,
            trace_id_high: self.trace_id_high,
            span_id: self.span_id,
            attributes: self.attributes.clone(),
            tracestate: self.tracestate.clone(),
            flags: self.flags,
        }
    }
}

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct SpanEvent<T: TraceData> {
    pub time_unix_nano: u64,
    pub name: T::Text,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<T::Text, AttributeAnyValue<T>>,
}

impl<T: TraceData> Clone for SpanEvent<T>
where
    T::Text: Clone,
{
    fn clone(&self) -> Self {
        SpanEvent {
            time_unix_nano: self.time_unix_nano,
            name: self.name.clone(),
            attributes: self.attributes.clone(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum AttributeAnyValue<T: TraceData> {
    SingleValue(AttributeArrayValue<T>),
    Array(Vec<AttributeArrayValue<T>>),
}

#[derive(Serialize)]
struct ArrayValueWrapper<'a, T: TraceData> {
    values: &'a Vec<AttributeArrayValue<T>>,
}

impl<T: TraceData> Serialize for AttributeAnyValue<T> {
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
                let wrapped_value = ArrayValueWrapper { values: value };
                state.serialize_field("array_value", &wrapped_value)?;
            }
        }

        state.end()
    }
}

impl<T: TraceData> From<&AttributeAnyValue<T>> for u8 {
    fn from(attribute: &AttributeAnyValue<T>) -> u8 {
        match attribute {
            AttributeAnyValue::SingleValue(value) => value.into(),
            AttributeAnyValue::Array(_) => 4,
        }
    }
}

impl<T: TraceData> Clone for AttributeAnyValue<T>
where
    T::Text: Clone,
{
    fn clone(&self) -> Self {
        match self {
            AttributeAnyValue::SingleValue(v) => AttributeAnyValue::SingleValue(v.clone()),
            AttributeAnyValue::Array(vec) => AttributeAnyValue::Array(vec.clone()),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum AttributeArrayValue<T: TraceData> {
    String(T::Text),
    Boolean(bool),
    Integer(i64),
    Double(f64),
}

impl<T: TraceData> Clone for AttributeArrayValue<T>
where
    T::Text: Clone,
{
    fn clone(&self) -> Self {
        match self {
            AttributeArrayValue::String(v) => AttributeArrayValue::String(v.clone()),
            AttributeArrayValue::Boolean(v) => AttributeArrayValue::Boolean(*v),
            AttributeArrayValue::Integer(v) => AttributeArrayValue::Integer(*v),
            AttributeArrayValue::Double(v) => AttributeArrayValue::Double(*v),
        }
    }
}

impl<T: TraceData> Serialize for AttributeArrayValue<T> {
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
    T: TraceData,
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

impl<T: TraceData> From<&AttributeArrayValue<T>> for u8 {
    fn from(attribute: &AttributeArrayValue<T>) -> u8 {
        match attribute {
            AttributeArrayValue::String(_) => 0,
            AttributeArrayValue::Boolean(_) => 1,
            AttributeArrayValue::Integer(_) => 2,
            AttributeArrayValue::Double(_) => 3,
        }
    }
}

fn is_default<T: Default + PartialEq>(t: &T) -> bool {
    t == &T::default()
}

pub type SpanBytes = Span<BytesData>;
pub type SpanLinkBytes = SpanLink<BytesData>;
pub type SpanEventBytes = SpanEvent<BytesData>;
pub type AttributeAnyValueBytes = AttributeAnyValue<BytesData>;
pub type AttributeArrayValueBytes = AttributeArrayValue<BytesData>;

pub type SpanSlice<'a> = Span<SliceData<'a>>;
pub type SpanLinkSlice<'a> = SpanLink<SliceData<'a>>;
pub type SpanEventSlice<'a> = SpanEvent<SliceData<'a>>;
pub type AttributeAnyValueSlice<'a> = AttributeAnyValue<SliceData<'a>>;
pub type AttributeArrayValueSlice<'a> = AttributeArrayValue<SliceData<'a>>;

pub type TraceChunksBytes = TraceChunks<BytesData>;

#[cfg(test)]
mod tests {
    use super::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink};
    use crate::msgpack_decoder::decode::buffer::Buffer;
    use crate::msgpack_decoder::v04::span::decode_span;
    use crate::span::SliceData;
    use std::collections::HashMap;

    #[test]
    fn skip_serializing_empty_fields_test() {
        let expected = b"\x87\xa7service\xa0\xa4name\xa0\xa8resource\xa0\xa8trace_id\x00\xa7span_id\x00\xa5start\x00\xa8duration\x00";
        let val: Span<SliceData<'_>> = Span::default();
        let serialized = rmp_serde::encode::to_vec_named(&val).unwrap();
        assert_eq!(expected, serialized.as_slice());
    }

    #[test]
    fn serialize_deserialize_test() {
        let span: Span<SliceData<'_>> = Span {
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
        let mut serialized_slice = Buffer::<SliceData<'_>>::new(serialized.as_ref());
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

        let span: Span<SliceData<'_>> = Span {
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
