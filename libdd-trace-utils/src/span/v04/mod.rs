// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::{BytesData, SliceData, SpanKeyParseError, TraceData, OwnedTraceData, TraceProjector, Traces, TraceAttributesOp, TraceAttributesMutOp, TraceAttributesMut, TraceAttributes, AttributeAnyContainer, AttributeAnyGetterContainer, AttributeAnySetterContainer, AttributeAnyValueType, TraceAttributesString, TraceAttributesBoolean, TraceAttributesInteger, TraceAttributesDouble, AttrRef, TraceDataLifetime, TracesMut, IntoData, SpanDataContents, parse_span_kind, span_kind_to_str};
use crate::tracer_payload::TraceChunks;
use serde::ser::SerializeStruct;
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::str::FromStr;
use std::hash::Hash;
use std::slice::Iter;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;

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
    #[serde(bound(serialize = "T::Text: Serialize"))]
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

// TraceProjector implementation for v04
type Trace<D> = Vec<Vec<Span<D>>>;
type Chunk<D> = Vec<Span<D>>;

/// TraceCollection is a wrapper for v04 traces that implements TraceProjector.
/// Unlike v05 which uses a shared dictionary, v04 stores strings directly in the spans.
pub struct TraceCollection<D: TraceData> {
    pub traces: Vec<Vec<Span<D>>>,
}

impl<D: TraceData> TraceCollection<D> {
    pub fn new(traces: Vec<Vec<Span<D>>>) -> Self {
        Self { traces }
    }
}

impl<D: TraceData> TraceProjector<D> for TraceCollection<D> {
    type Storage<'a> = () where D: 'a;
    type Trace<'a> = Trace<D> where D: 'a;
    type Chunk<'a> = Chunk<D> where D: 'a;
    type Span<'a> = Span<D> where D: 'a;
    type SpanLink<'a> = SpanLink<D> where D: 'a;
    type SpanEvent<'a> = SpanEvent<D> where D: 'a;

    fn project<'a>(&'a self) -> Traces<Self, D> {
        Traces::new(&self.traces, &())
    }

    fn project_mut<'a>(&'a mut self) -> TracesMut<Self, D> {
        // For v04, storage is unit type (), which is zero-sized
        // We can safely create a mutable reference to a local unit value
        // because it doesn't actually store anything
        unsafe {
            Traces::new_mut(&mut self.traces, std::mem::transmute::<&mut (), &'a mut ()>(&mut ()))
        }
    }

    fn add_chunk<'a>(trace: &'a mut Trace<D>, _storage: &mut ()) -> &'a mut Chunk<D> {
        trace.push(Vec::new());
        unsafe { trace.last_mut().unwrap_unchecked() }
    }

    fn chunk_iterator<'a>(trace: &'a Trace<D>) -> Iter<'a, Chunk<D>> {
        trace.iter()
    }

    fn retain_chunks<'b, 'a, F: for<'c> FnMut(&'c mut Self::Chunk<'c>, &'c mut Self::Storage<'a>) -> bool>(trace: &'b mut Self::Trace<'b>, storage: &'a mut Self::Storage<'a>, mut predicate: F) {
        trace.retain_mut(move |chunk| predicate(chunk, storage))
    }

    fn add_span<'a>(chunk: &'a mut Chunk<D>, _storage: &mut ()) -> &'a mut Span<D> {
        chunk.push(Span::default());
        let trace_id = chunk.first().map(|s| s.trace_id).unwrap_or(0);
        let span = unsafe { chunk.last_mut().unwrap_unchecked() };
        span.trace_id = trace_id;
        span
    }

    fn span_iterator<'a>(chunk: &'a Chunk<D>) -> Iter<'a, Span<D>> {
        chunk.iter()
    }

    fn retain_spans<'r, F: FnMut(&mut Self::Span<'r>, &mut Self::Storage<'r>) -> bool>(chunk: &'r mut Self::Chunk<'r>, storage: &'r mut Self::Storage<'r>, mut predicate: F) {
        chunk.retain_mut(|span| predicate(span, storage))
    }

    fn add_span_link<'a>(span: &'a mut Span<D>, _storage: &mut ()) -> &'a mut SpanLink<D> {
        span.span_links.push(SpanLink::default());
        unsafe { span.span_links.last_mut().unwrap_unchecked() }
    }

    fn span_link_iterator<'a>(span: &'a Span<D>) -> Iter<'a, SpanLink<D>> {
        span.span_links.iter()
    }

    fn retain_span_links<'r, F: FnMut(&mut Self::SpanLink<'r>, &mut Self::Storage<'r>) -> bool>(span: &'r mut Self::Span<'r>, storage: &'r mut Self::Storage<'r>, mut predicate: F) {
        span.span_links.retain_mut(|link| predicate(link, storage))
    }

    fn add_span_event<'a>(span: &mut Self::Span<'a>, _storage: &mut Self::Storage<'a>) -> &'a mut Self::SpanEvent<'a> {
        span.span_events.push(SpanEvent::default());
        // SAFETY: We just pushed an element, so last_mut() will return Some
        // The lifetime 'a is tied to the span parameter through Self::Span<'a>
        unsafe { std::mem::transmute(span.span_events.last_mut().unwrap_unchecked()) }
    }

    fn span_event_iterator<'a>(span: &'a Span<D>) -> Iter<'a, SpanEvent<D>> {
        span.span_events.iter()
    }

    fn retain_span_events<'r, F: FnMut(&mut Self::SpanEvent<'r>, &mut Self::Storage<'r>) -> bool>(span: &'r mut Self::Span<'r>, storage: &'r mut Self::Storage<'r>, mut predicate: F) {
        span.span_events.retain_mut(|event| predicate(event, storage))
    }

    // Trace-level getters - v04 doesn't have trace-level attributes, return defaults
    fn get_trace_container_id<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_language_name<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_language_version<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_tracer_version<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_runtime_id<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_env<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_hostname<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn get_trace_app_version<'a>(_trace: &Trace<D>, _storage: &'a ()) -> &'a D::Text {
        D::Text::default_ref()
    }

    // Trace-level setters - v04 doesn't have trace-level attributes, do nothing
    fn set_trace_container_id(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_language_name(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_language_version(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_tracer_version(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_runtime_id(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_env(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_hostname(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}
    fn set_trace_app_version(_trace: &mut Trace<D>, _storage: &mut (), _value: D::Text) {}

    // Chunk-level getters
    fn get_chunk_priority(chunk: &Chunk<D>, _storage: &()) -> i32 {
        chunk.first()
            .and_then(|span| span.metrics.get("_sampling_priority_v1"))
            .copied()
            .unwrap_or(0.0) as i32
    }

    fn get_chunk_origin<'a>(chunk: &Self::Chunk<'_>, _storage: &'a ()) -> &'a D::Text {
        // SAFETY: For v04, data lives in the chunk, not storage
        // We extend the lifetime from the chunk to 'a (which is tied to storage)
        unsafe {
            std::mem::transmute(
                chunk.first()
                    .and_then(|span| span.meta.get("_dd.origin"))
                    .or_else(|| chunk.first().map(|s| &s.service))
                    .unwrap_or(D::Text::default_ref())
            )
        }
    }

    fn get_chunk_dropped_trace(_chunk: &Chunk<D>, _storage: &()) -> bool {
        false
    }

    fn get_chunk_trace_id(chunk: &Chunk<D>, _storage: &()) -> u128 {
        chunk.first().map(|span| span.trace_id).unwrap_or(0)
    }

    fn get_chunk_sampling_mechanism(chunk: &Chunk<D>, _storage: &()) -> u32 {
        chunk.first()
            .and_then(|span| span.metrics.get("_dd.span_sampling.mechanism"))
            .copied()
            .unwrap_or(0.0) as u32
    }

    // Chunk-level setters
    fn set_chunk_priority(chunk: &mut Chunk<D>, _storage: &mut (), value: i32) {
        if let Some(span) = chunk.first_mut() {
            span.metrics.insert(IntoData::<D::Text>::into("_sampling_priority_v1"), value as f64);
        }
    }

    fn set_chunk_origin(chunk: &mut Chunk<D>, _storage: &mut (), value: D::Text) {
        if let Some(span) = chunk.first_mut() {
            span.meta.insert(IntoData::<D::Text>::into("_dd.origin"), value);
        }
    }

    fn set_chunk_dropped_trace(_chunk: &mut Chunk<D>, _storage: &mut (), _value: bool) {}

    fn set_chunk_trace_id(chunk: &mut Chunk<D>, _storage: &mut (), value: u128) where D: OwnedTraceData {
        for span in chunk.iter_mut() {
            span.trace_id = value;
        }
    }

    fn set_chunk_sampling_mechanism(chunk: &mut Chunk<D>, _storage: &mut (), value: u32) {
        if let Some(span) = chunk.first_mut() {
            span.metrics.insert(IntoData::<D::Text>::into("_dd.span_sampling.mechanism"), value as f64);
        }
    }

    // Span-level getters
    // For v04, data lives in the span itself, not storage
    // We must match the trait signature which expects lifetime 'a from storage
    // We extend the lifetime from the span to match the storage lifetime
    fn get_span_service<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&span.service) }
    }

    fn get_span_name<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&span.name) }
    }

    fn get_span_resource<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&span.resource) }
    }

    fn get_span_type<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&span.r#type) }
    }

    fn get_span_span_id(span: &Span<D>, _storage: &()) -> u64 {
        span.span_id
    }

    fn get_span_parent_id(span: &Span<D>, _storage: &()) -> u64 {
        span.parent_id
    }

    fn get_span_start(span: &Span<D>, _storage: &()) -> i64 {
        span.start
    }

    fn get_span_duration(span: &Span<D>, _storage: &()) -> i64 {
        span.duration
    }

    fn get_span_error(span: &Span<D>, _storage: &()) -> bool {
        span.error != 0
    }

    fn get_span_env<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(span.meta.get("env").unwrap_or(&span.service)) }
    }

    fn get_span_version<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(span.meta.get("version").unwrap_or(&span.service)) }
    }

    fn get_span_component<'a>(span: &Self::Span<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(span.meta.get("component").unwrap_or(&span.service)) }
    }

    fn get_span_kind(span: &Span<D>, _storage: &()) -> SpanKind {
        let kind = span.meta.get("kind");
        parse_span_kind(kind.map(|k| k.borrow()).unwrap_or(""))
    }

    // Span-level setters
    fn set_span_service(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.service = value;
    }

    fn set_span_name(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.name = value;
    }

    fn set_span_resource(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.resource = value;
    }

    fn set_span_type(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.r#type = value;
    }

    fn set_span_span_id(span: &mut Span<D>, _storage: &mut (), value: u64) {
        span.span_id = value;
    }

    fn set_span_parent_id(span: &mut Span<D>, _storage: &mut (), value: u64) {
        span.parent_id = value;
    }

    fn set_span_start(span: &mut Span<D>, _storage: &mut (), value: i64) {
        span.start = value;
    }

    fn set_span_duration(span: &mut Span<D>, _storage: &mut (), value: i64) {
        span.duration = value;
    }

    fn set_span_error(span: &mut Span<D>, _storage: &mut (), value: bool) {
        span.error = value as i32;
    }

    fn set_span_env(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.meta.insert(IntoData::<D::Text>::into("env"), value);
    }

    fn set_span_version(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.meta.insert(IntoData::<D::Text>::into("version"), value);
    }

    fn set_span_component(span: &mut Span<D>, _storage: &mut (), value: D::Text) {
        span.meta.insert(IntoData::<D::Text>::into("component"), value);
    }

    fn set_span_kind(span: &mut Span<D>, _storage: &mut (), value: SpanKind) {
        match span_kind_to_str(value) {
            Some(kind) => { span.meta.insert(IntoData::<D::Text>::into("kind"), IntoData::<D::Text>::into(kind)); },
            None => { span.meta.remove("kind"); },
        }
    }

    // SpanLink getters
    fn get_link_trace_id(link: &SpanLink<D>, _storage: &()) -> u128 {
        (link.trace_id_high as u128) << 64 | link.trace_id as u128
    }

    fn get_link_span_id(link: &SpanLink<D>, _storage: &()) -> u64 {
        link.span_id
    }

    fn get_link_trace_state<'a>(link: &Self::SpanLink<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&link.tracestate) }
    }

    fn get_link_flags(link: &SpanLink<D>, _storage: &()) -> u32 {
        link.flags
    }

    // SpanLink setters
    fn set_link_trace_id(link: &mut SpanLink<D>, _storage: &mut (), value: u128) {
        link.trace_id = value as u64;
        link.trace_id_high = (value >> 64) as u64;
    }

    fn set_link_span_id(link: &mut SpanLink<D>, _storage: &mut (), value: u64) {
        link.span_id = value;
    }

    fn set_link_trace_state(link: &mut SpanLink<D>, _storage: &mut (), value: D::Text) {
        link.tracestate = value;
    }

    fn set_link_flags(link: &mut SpanLink<D>, _storage: &mut (), value: u32) {
        link.flags = value;
    }

    // SpanEvent getters
    fn get_event_time_unix_nano(event: &SpanEvent<D>, _storage: &()) -> u64 {
        event.time_unix_nano
    }

    fn get_event_name<'a>(event: &Self::SpanEvent<'_>, _storage: &'a ()) -> &'a D::Text {
        unsafe { std::mem::transmute(&event.name) }
    }

    // SpanEvent setters
    fn set_event_time_unix_nano(event: &mut SpanEvent<D>, _storage: &mut (), value: u64) {
        event.time_unix_nano = value;
    }

    fn set_event_name(event: &mut SpanEvent<D>, _storage: &mut (), value: D::Text) {
        event.name = value;
    }
}

// Attribute operations for Span
// For v04, data lives in the span, so 'b (container lifetime) must outlive 'a (storage lifetime)
impl<'a, 'b, D: TraceData, const Mut: u8> TraceAttributesOp<'b, 'a, TraceCollection<D>, D, Span<D>> for TraceAttributes<'a, TraceCollection<D>, D, AttrRef<'b, Span<D>>, Span<D>, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'b Span<D>, _storage: &'a (), key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, TraceCollection<D>, D, Span<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        // For v04, HashMap uses D::Text as keys directly
        // We need to iterate and find a match using Equivalent
        for (k, v) in &container.meta {
            if key.equivalent(&k.as_ref_copy()) {
                // SAFETY: In v04, data is stored directly in containers, not in storage.
                // We transmute the lifetime from 'b to 'a for the return value.
                let v_with_storage_lifetime: &'a D::Text = unsafe { std::mem::transmute(v) };
                return Some(AttributeAnyContainer::String(v_with_storage_lifetime));
            }
        }
        for (k, v) in &container.metrics {
            if key.equivalent(&k.as_ref_copy()) {
                return Some(AttributeAnyContainer::Double(*v));
            }
        }
        None
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TraceCollection<D>, D, Span<D>> for TraceAttributesMut<'a, TraceCollection<D>, D, AttrRef<'b, Span<D>>, Span<D>> {
    type MutString = &'b mut D::Text;
    type MutBytes = ();
    type MutBoolean = &'b mut f64;
    type MutInteger = &'b mut f64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut Span<D>, _storage: &mut (), key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Span<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        // Try to find in meta
        for (k, v) in &mut container.meta {
            if key.equivalent(&k.as_ref_copy()) {
                return Some(AttributeAnyContainer::String(v));
            }
        }
        // Try to find in metrics
        for (k, v) in &mut container.metrics {
            if key.equivalent(&k.as_ref_copy()) {
                return Some(AttributeAnyContainer::Double(v));
            }
        }
        None
    }

    fn set(container: &'b mut Span<D>, _storage: &mut (), key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Span<D>> {
        use std::collections::hash_map::Entry;
        match value {
            AttributeAnyValueType::String => {
                let entry = container.meta.entry(key).or_insert_with(D::Text::default);
                AttributeAnyContainer::String(entry)
            },
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean | AttributeAnyValueType::Integer | AttributeAnyValueType::Double => {
                let entry = container.metrics.entry(key).or_insert(0.0);
                AttributeAnyContainer::Double(entry)
            },
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut Span<D>, _storage: &mut (), key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        // For std HashMap with owned keys, we use retain which allows us to check without cloning
        container.meta.retain(|k, _| !key.equivalent(&k.as_ref_copy()));
        container.metrics.retain(|k, _| !key.equivalent(&k.as_ref_copy()));
    }
}

// Trait implementations for mutable references
impl<'a, 'b, D: TraceData> TraceAttributesString<'a, TraceCollection<D>, D> for &'b mut D::Text {
    fn get(&self, _storage: &'a ()) -> &'a D::Text {
        // SAFETY: In v04, data is stored directly in containers, not in storage.
        // We transmute the lifetime from 'b to 'a.
        unsafe { std::mem::transmute::<&D::Text, &'a D::Text>(*self) }
    }

    fn set(self, _storage: &mut (), value: D::Text) {
        *self = value;
    }
}

// Note: TraceAttributesBoolean, TraceAttributesInteger, and TraceAttributesDouble for &mut f64
// are already implemented in v05/mod.rs and apply to both v04 and v05

// Empty implementations for types that don't have attributes
impl<'a, 'b, D: TraceData, const Mut: u8> TraceAttributesOp<'b, 'a, TraceCollection<D>, D, Trace<D>> for TraceAttributes<'a, TraceCollection<D>, D, AttrRef<'b, Trace<D>>, Trace<D>, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b Trace<D>, _storage: &'a (), _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, TraceCollection<D>, D, Trace<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TraceCollection<D>, D, Trace<D>> for TraceAttributesMut<'a, TraceCollection<D>, D, AttrRef<'b, Trace<D>>, Trace<D>> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut Trace<D>, _storage: &mut (), _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Trace<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut Trace<D>, _storage: &mut (), _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Trace<D>> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut Trace<D>, _storage: &mut (), _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

impl<'a, 'b, D: TraceData, const Mut: u8> TraceAttributesOp<'b, 'a, TraceCollection<D>, D, Chunk<D>> for TraceAttributes<'a, TraceCollection<D>, D, AttrRef<'b, Chunk<D>>, Chunk<D>, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b Chunk<D>, _storage: &'a (), _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, TraceCollection<D>, D, Chunk<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TraceCollection<D>, D, Chunk<D>> for TraceAttributesMut<'a, TraceCollection<D>, D, AttrRef<'b, Chunk<D>>, Chunk<D>> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut Chunk<D>, _storage: &mut (), _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Chunk<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut Chunk<D>, _storage: &mut (), _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, Chunk<D>> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut Chunk<D>, _storage: &mut (), _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

impl<'a, 'b, D: TraceData, const Mut: u8> TraceAttributesOp<'b, 'a, TraceCollection<D>, D, SpanLink<D>> for TraceAttributes<'a, TraceCollection<D>, D, AttrRef<'b, SpanLink<D>>, SpanLink<D>, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'b SpanLink<D>, _storage: &'a (), key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanLink<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        for (k, v) in &container.attributes {
            if key.equivalent(&k.as_ref_copy()) {
                // SAFETY: In v04, data is stored directly in containers, not in storage.
                // We transmute the lifetime from 'b to 'a for the return value.
                let v_with_storage_lifetime: &'a D::Text = unsafe { std::mem::transmute(v) };
                return Some(AttributeAnyContainer::String(v_with_storage_lifetime));
            }
        }
        None
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TraceCollection<D>, D, SpanLink<D>> for TraceAttributesMut<'a, TraceCollection<D>, D, AttrRef<'b, SpanLink<D>>, SpanLink<D>> {
    type MutString = &'b mut D::Text;
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut SpanLink<D>, _storage: &mut (), key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanLink<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        for (k, v) in &mut container.attributes {
            if key.equivalent(&k.as_ref_copy()) {
                return Some(AttributeAnyContainer::String(v));
            }
        }
        None
    }

    fn set(container: &'b mut SpanLink<D>, _storage: &mut (), key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanLink<D>> {
        use std::collections::hash_map::Entry;
        match value {
            AttributeAnyValueType::String => {
                let entry = container.attributes.entry(key).or_insert_with(D::Text::default);
                AttributeAnyContainer::String(entry)
            },
            _ => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut SpanLink<D>, _storage: &mut (), key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        container.attributes.retain(|k, _| !key.equivalent(&k.as_ref_copy()));
    }
}

impl<'a, 'b, D: TraceData, const Mut: u8> TraceAttributesOp<'b, 'a, TraceCollection<D>, D, SpanEvent<D>> for TraceAttributes<'a, TraceCollection<D>, D, AttrRef<'b, SpanEvent<D>>, SpanEvent<D>, Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'b SpanEvent<D>, _storage: &'a (), _key: &K) -> Option<AttributeAnyGetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanEvent<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        // SpanEvent attributes are stored in a different format (AttributeAnyValue)
        // For now, return None - full implementation would need to convert AttributeAnyValue
        None
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TraceCollection<D>, D, SpanEvent<D>> for TraceAttributesMut<'a, TraceCollection<D>, D, AttrRef<'b, SpanEvent<D>>, SpanEvent<D>> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'b mut SpanEvent<D>, _storage: &mut (), _key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanEvent<D>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }

    fn set(_container: &'b mut SpanEvent<D>, _storage: &mut (), _key: D::Text, _value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TraceCollection<D>, D, SpanEvent<D>> {
        AttributeAnyContainer::Map(())
    }

    fn remove<K>(_container: &mut SpanEvent<D>, _storage: &mut (), _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

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
