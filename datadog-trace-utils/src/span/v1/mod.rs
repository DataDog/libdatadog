use std::borrow::Borrow;
use std::collections::HashMap;
use datadog_trace_protobuf::pb::idx::SpanKind;
use crate::span::{BytesData, SliceData, TraceData, table::*};



/// Checks if the `value` represents an empty string. Used to skip serializing empty strings
/// with serde.
fn is_empty_str<T: Borrow<str>>(value: &T) -> bool {
    value.borrow().is_empty()
}

#[derive(Default)]
struct TraceStaticData<T: TraceData> {
    pub strings: StaticDataVec<T, TraceDataText>,
    pub bytes: StaticDataVec<T, TraceDataBytes>,
}

impl<T: TraceData> TraceStaticData<T> {
    pub fn get_string(&self, r#ref: TraceStringRef) -> &T::Text {
        self.strings.get(r#ref)
    }

    pub fn add_string(&mut self, value: T::Text) -> TraceStringRef {
        self.strings.add(value)
    }

    pub fn get_bytes(&self, r#ref: TraceBytesRef) -> &T::Bytes {
        self.bytes.get(r#ref)
    }

    pub fn add_bytes(&mut self, value: T::Bytes) -> TraceBytesRef {
        self.bytes.add(value)
    }
}

#[derive(Default, Debug)]
pub struct Traces<T: TraceData> {
    pub static_data: TraceStaticData<T>,
    pub container_id: TraceStringRef,
    pub language_name: TraceStringRef,
    pub language_version: TraceStringRef,
    pub tracer_version: TraceStringRef,
    pub runtime_id: TraceStringRef,
    pub env: TraceStringRef,
    pub hostname: TraceStringRef,
    pub app_version: TraceStringRef,
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    pub chunks: Vec<TraceChunk>,
}

#[derive(Debug, Default)]
pub struct TraceChunk {
    pub priority: i32,
    pub origin: TraceStringRef,
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    pub spans: Vec<Span>,
    pub dropped_trace: bool,
    pub trace_id: u128,
    pub sampling_mechanism: u32,
}

/// The generic representation of a V04 span.
///
/// `T` is the type used to represent strings in the span, it can be either owned (e.g. BytesString)
/// or borrowed (e.g. &str). To define a generic function taking any `Span<T>` you can use the
/// [`SpanValue`] trait:
/// ```
/// use datadog_trace_utils::span::{Span, SpanText};
/// fn foo<T: SpanText>(span: Span<T>) {
///     let _ = span.attributes.get("foo");
/// }
/// ```
#[derive(Debug, Default, PartialEq)]
pub struct Span {
    pub service: TraceStringRef,
    pub name: TraceStringRef,
    pub resource: TraceStringRef,
    #[serde(skip_serializing_if = "is_empty_str")]
    pub r#type: TraceStringRef,
    pub span_id: u64,
    #[serde(skip_serializing_if = "is_default")]
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    #[serde(skip_serializing_if = "is_default")]
    pub error: bool,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_links: Vec<SpanLink>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub span_events: Vec<SpanEvent>,
    pub env: TraceStringRef,
    pub version: TraceStringRef,
    pub component: TraceStringRef,
    pub kind: SpanKind,
}

/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug, Default, PartialEq)]
pub struct SpanLink {
    pub trace_id: u128,
    pub span_id: u64,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    #[serde(skip_serializing_if = "is_empty_str")]
    pub tracestate: TraceStringRef,
    #[serde(skip_serializing_if = "is_default")]
    pub flags: u32,
}

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug, Default, PartialEq)]
pub struct SpanEvent {
    pub time_unix_nano: u64,
    pub name: TraceStringRef,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
}

#[derive(Debug, PartialEq)]
pub enum AttributeAnyValue {
    String(TraceStringRef),
    Bytes(TraceBytesRef),
    Boolean(bool),
    Integer(i64),
    Double(f64),
    Array(Vec<AttributeAnyValue>),
    Map(HashMap<TraceStringRef, AttributeAnyValue>)
}

pub type TracesSlice<'a> = Traces<SliceData<'a>>;
pub type TracesBytes = Traces<BytesData>;
