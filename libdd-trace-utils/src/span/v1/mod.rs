use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{BytesData, SliceData, TraceData, OwnedTraceData, TraceDataLifetime, SpanDataContents, AttributeAnyContainer, AttributeAnySetterContainer, AttrRef, TraceAttributesMut, TraceAttributesMutOp, TraceAttributesString, TraceAttributesInteger, TraceAttributesBoolean, AttributeAnyGetterContainer, TraceAttributes, TraceAttributesOp, TracesMut, Traces as TracesStruct, TraceProjector, AttributeAnyValueType};
use crate::span::table::{TraceBytesRef, TraceDataText, TraceDataBytes, TraceDataRef, TraceStringRef, StaticDataVec};



/// Checks if the `value` represents an empty string. Used to skip serializing empty strings
/// with serde.
fn is_empty_str<T: Borrow<str>>(value: &T) -> bool {
    value.borrow().is_empty()
}

#[derive(Default, Debug)]
pub struct TraceStaticData<T: TraceData> {
    pub strings: StaticDataVec<T, TraceDataText>,
    pub bytes: StaticDataVec<T, TraceDataBytes>,
}

impl<T: TraceData> TraceStaticData<T> {
    pub fn get_string(&self, r#ref: TraceStringRef) -> &T::Text {
        StaticDataVec::get(&self.strings, r#ref)
    }

    pub fn get_bytes(&self, r#ref: TraceBytesRef) -> &T::Bytes {
        StaticDataVec::get(&self.bytes, r#ref)
    }

    pub fn add_string(&mut self, value: T::Text) -> TraceStringRef {
        self.strings.add(value)
    }

    pub fn add_bytes(&mut self, value: T::Bytes) -> TraceBytesRef {
        self.bytes.add(value)
    }
}

// We split this struct so that we can borrow the byte/string data separately from traces data
#[derive(Default, Debug)]
pub struct TracePayload<T: TraceData> {
    pub static_data: TraceStaticData<T>,
    pub traces: Traces,
}

#[derive(Default, Debug)]
pub struct Traces {
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
    pub r#type: TraceStringRef,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: bool,
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    pub span_links: Vec<SpanLink>,
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
    pub attributes: HashMap<TraceStringRef, AttributeAnyValue>,
    pub tracestate: TraceStringRef,
    pub flags: u32,
}

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug, Default, PartialEq)]
pub struct SpanEvent {
    pub time_unix_nano: u64,
    pub name: TraceStringRef,
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

pub type TracePayloadSlice<'a> = TracePayload<SliceData<'a>>;
pub type TracePayloadBytes = TracePayload<BytesData>;

// TraceProjector implementation for v1
impl<'s, D: TraceDataLifetime<'s>> TraceProjector<'s, D> for TracePayload<D> where D: 's {
    type Storage = TraceStaticData<D>;
    type Trace = Traces;
    type Chunk = TraceChunk;
    type Span = Span;
    type SpanLink = SpanLink;
    type SpanEvent = SpanEvent;

    fn project(&'s self) -> TracesStruct<'s, Self, D> {
        TracesStruct::new(&self.traces, &self.static_data)
    }

    fn project_mut(&'s mut self) -> TracesMut<'s, Self, D> {
        TracesMut::new_mut(&mut self.traces, &mut self.static_data)
    }

    // Trace-level getters
    fn get_trace_container_id(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.container_id)
    }

    fn get_trace_language_name(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.language_name)
    }

    fn get_trace_language_version(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.language_version)
    }

    fn get_trace_tracer_version(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.tracer_version)
    }

    fn get_trace_runtime_id(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.runtime_id)
    }

    fn get_trace_env(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.env)
    }

    fn get_trace_hostname(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.hostname)
    }

    fn get_trace_app_version(trace: &Traces, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(trace.app_version)
    }

    // Trace-level setters
    fn set_trace_container_id(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.container_id = storage.add_string(value);
    }

    fn set_trace_language_name(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.language_name = storage.add_string(value);
    }

    fn set_trace_language_version(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.language_version = storage.add_string(value);
    }

    fn set_trace_tracer_version(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.tracer_version = storage.add_string(value);
    }

    fn set_trace_runtime_id(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.runtime_id = storage.add_string(value);
    }

    fn set_trace_env(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.env = storage.add_string(value);
    }

    fn set_trace_hostname(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.hostname = storage.add_string(value);
    }

    fn set_trace_app_version(trace: &mut Traces, storage: &mut TraceStaticData<D>, value: D::Text) {
        trace.app_version = storage.add_string(value);
    }

    // Chunk-level getters
    fn get_chunk_priority<'a>(chunk: &'a TraceChunk, _storage: &'a TraceStaticData<D>) -> i32 {
        chunk.priority
    }

    fn get_chunk_origin(chunk: &'s TraceChunk, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(chunk.origin)
    }

    fn get_chunk_dropped_trace<'a>(chunk: &'a TraceChunk, _storage: &'a TraceStaticData<D>) -> bool {
        chunk.dropped_trace
    }

    fn get_chunk_trace_id<'a>(chunk: &'a TraceChunk, _storage: &'a TraceStaticData<D>) -> u128 {
        chunk.trace_id
    }

    fn get_chunk_sampling_mechanism<'a>(chunk: &'a TraceChunk, _storage: &'a TraceStaticData<D>) -> u32 {
        chunk.sampling_mechanism
    }

    fn set_chunk_priority(chunk: &mut TraceChunk, _storage: &mut TraceStaticData<D>, value: i32) {
        chunk.priority = value;
    }

    fn set_chunk_origin(chunk: &mut TraceChunk, storage: &mut TraceStaticData<D>, value: D::Text) {
        chunk.origin = storage.add_string(value);
    }

    fn set_chunk_dropped_trace(chunk: &mut TraceChunk, _storage: &mut TraceStaticData<D>, value: bool) {
        chunk.dropped_trace = value;
    }

    fn set_chunk_trace_id(chunk: &mut TraceChunk, _storage: &mut TraceStaticData<D>, value: u128) where D: OwnedTraceData {
        chunk.trace_id = value;
    }

    fn set_chunk_sampling_mechanism(chunk: &mut TraceChunk, _storage: &mut TraceStaticData<D>, value: u32) {
        chunk.sampling_mechanism = value;
    }

    // Span-level getters
    fn get_span_service(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.service)
    }

    fn get_span_name(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.name)
    }

    fn get_span_resource(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.resource)
    }

    fn get_span_type(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.r#type)
    }

    fn get_span_span_id(span: &Span, _storage: &TraceStaticData<D>) -> u64 {
        span.span_id
    }

    fn get_span_parent_id(span: &Span, _storage: &TraceStaticData<D>) -> u64 {
        span.parent_id
    }

    fn get_span_start(span: &Span, _storage: &TraceStaticData<D>) -> i64 {
        span.start
    }

    fn get_span_duration(span: &Span, _storage: &TraceStaticData<D>) -> i64 {
        span.duration
    }

    fn get_span_error(span: &Span, _storage: &TraceStaticData<D>) -> bool {
        span.error
    }

    fn get_span_env(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.env)
    }

    fn get_span_version(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.version)
    }

    fn get_span_component(span: &'s Span, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(span.component)
    }

    fn get_span_kind(span: &Span, _storage: &TraceStaticData<D>) -> SpanKind {
        span.kind
    }

    // Span-level setters
    fn set_span_service(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.service = storage.add_string(value);
    }

    fn set_span_name(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.name = storage.add_string(value);
    }

    fn set_span_resource(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.resource = storage.add_string(value);
    }

    fn set_span_type(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.r#type = storage.add_string(value);
    }

    fn set_span_span_id(span: &mut Span, _storage: &mut TraceStaticData<D>, value: u64) {
        span.span_id = value;
    }

    fn set_span_parent_id(span: &mut Span, _storage: &mut TraceStaticData<D>, value: u64) {
        span.parent_id = value;
    }

    fn set_span_start(span: &mut Span, _storage: &mut TraceStaticData<D>, value: i64) {
        span.start = value;
    }

    fn set_span_duration(span: &mut Span, _storage: &mut TraceStaticData<D>, value: i64) {
        span.duration = value;
    }

    fn set_span_error(span: &mut Span, _storage: &mut TraceStaticData<D>, value: bool) {
        span.error = value;
    }

    fn set_span_env(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.env = storage.add_string(value);
    }

    fn set_span_version(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.version = storage.add_string(value);
    }

    fn set_span_component(span: &mut Span, storage: &mut TraceStaticData<D>, value: D::Text) {
        span.component = storage.add_string(value);
    }

    fn set_span_kind(span: &mut Span, _storage: &mut TraceStaticData<D>, value: SpanKind) {
        span.kind = value;
    }

    // SpanLink getters
    fn get_link_trace_id(link: &'s SpanLink, _storage: &'s TraceStaticData<D>) -> u128 {
        link.trace_id
    }

    fn get_link_span_id(link: &'s SpanLink, _storage: &'s TraceStaticData<D>) -> u64 {
        link.span_id
    }

    fn get_link_trace_state(link: &'s SpanLink, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(link.tracestate)
    }

    fn get_link_flags(link: &'s SpanLink, _storage: &'s TraceStaticData<D>) -> u32 {
        link.flags
    }

    // SpanLink setters
    fn set_link_trace_id(link: &mut SpanLink, _storage: &mut TraceStaticData<D>, value: u128) {
        link.trace_id = value;
    }

    fn set_link_span_id(link: &mut SpanLink, _storage: &mut TraceStaticData<D>, value: u64) {
        link.span_id = value;
    }

    fn set_link_trace_state(link: &mut SpanLink, storage: &mut TraceStaticData<D>, value: D::Text) {
        link.tracestate = storage.add_string(value);
    }

    fn set_link_flags(link: &mut SpanLink, _storage: &mut TraceStaticData<D>, value: u32) {
        link.flags = value;
    }

    // SpanEvent getters
    fn get_event_time_unix_nano(event: &'s SpanEvent, _storage: &'s TraceStaticData<D>) -> u64 {
        event.time_unix_nano
    }

    fn get_event_name(event: &'s SpanEvent, storage: &'s TraceStaticData<D>) -> &'s D::Text {
        storage.get_string(event.name)
    }

    // SpanEvent setters
    fn set_event_time_unix_nano(event: &mut SpanEvent, _storage: &mut TraceStaticData<D>, value: u64) {
        event.time_unix_nano = value;
    }

    fn set_event_name(event: &mut SpanEvent, storage: &mut TraceStaticData<D>, value: D::Text) {
        event.name = storage.add_string(value);
    }

    // Collection management methods
    fn add_chunk<'b>(trace: &'b mut Traces, _storage: &mut TraceStaticData<D>) -> &'b mut TraceChunk {
        trace.chunks.push(TraceChunk::default());
        trace.chunks.last_mut().unwrap()
    }

    fn chunk_iterator(trace: &'s Traces) -> std::slice::Iter<'s, TraceChunk> {
        trace.chunks.iter()
    }

    fn retain_chunks<'b, F: for<'c> FnMut(&'c mut TraceChunk, &'c mut TraceStaticData<D>) -> bool>(
        trace: &'b mut Traces,
        storage: &'b mut TraceStaticData<D>,
        mut predicate: F
    ) {
        trace.chunks.retain_mut(|chunk| predicate(chunk, storage));
    }

    fn add_span<'b>(chunk: &'b mut TraceChunk, _storage: &mut TraceStaticData<D>) -> &'b mut Span {
        chunk.spans.push(Span::default());
        chunk.spans.last_mut().unwrap()
    }

    fn span_iterator(chunk: &'s TraceChunk) -> std::slice::Iter<'s, Span> {
        chunk.spans.iter()
    }

    fn retain_spans<'b, F: FnMut(&mut Span, &mut TraceStaticData<D>) -> bool>(
        chunk: &'b mut TraceChunk,
        storage: &'b mut TraceStaticData<D>,
        mut predicate: F
    ) {
        chunk.spans.retain_mut(|span| predicate(span, storage));
    }

    fn add_span_link<'b>(span: &'b mut Span, _storage: &mut TraceStaticData<D>) -> &'b mut SpanLink {
        span.span_links.push(SpanLink::default());
        span.span_links.last_mut().unwrap()
    }

    fn span_link_iterator(span: &'s Span) -> std::slice::Iter<'s, SpanLink> {
        span.span_links.iter()
    }

    fn retain_span_links<'b, F: FnMut(&mut SpanLink, &mut TraceStaticData<D>) -> bool>(
        span: &'b mut Span,
        storage: &'b mut TraceStaticData<D>,
        mut predicate: F
    ) {
        span.span_links.retain_mut(|link| predicate(link, storage));
    }

    fn add_span_event<'b>(span: &'b mut Span, _storage: &mut TraceStaticData<D>) -> &'b mut SpanEvent {
        span.span_events.push(SpanEvent::default());
        span.span_events.last_mut().unwrap()
    }

    fn span_event_iterator(span: &'s Span) -> std::slice::Iter<'s, SpanEvent> {
        span.span_events.iter()
    }

    fn retain_span_events<'b, F: FnMut(&mut SpanEvent, &mut TraceStaticData<D>) -> bool>(
        span: &'b mut Span,
        storage: &'b mut TraceStaticData<D>,
        mut predicate: F
    ) {
        span.span_events.retain_mut(|event| predicate(event, storage));
    }
}

// Helper trait for finding string refs in HashMap
trait HashMapFind<D: TraceData> {
    fn find<K>(&self, key: &K) -> Option<TraceStringRef>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
}

impl<D: TraceData> HashMapFind<D> for TraceStaticData<D> {
    fn find<K>(&self, key: &K) -> Option<TraceStringRef>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        // Use the StaticDataVec's find method for fast lookup
        self.strings.find(key)
    }
}

// TraceAttributesOp implementation for Traces
impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, Traces> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Traces>, Traces, ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'a Traces, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, Traces>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| match v {
            AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
            AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }
}

// Similar implementations for TraceChunk, Span, SpanLink, SpanEvent
impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, TraceChunk> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, TraceChunk>, TraceChunk, ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'a TraceChunk, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, TraceChunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| match v {
            AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
            AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, Span> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Span>, Span, ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'a Span, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| match v {
            AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
            AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, SpanLink> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanLink>, SpanLink, ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'a SpanLink, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, SpanLink>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| match v {
            AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
            AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, SpanEvent> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanEvent>, SpanEvent, ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(container: &'a SpanEvent, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, SpanEvent>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| match v {
            AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
            AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }
}

// Helper type for mutable attribute references in v1
type MutableAttrRef = TraceStringRef;

// Trait implementations for mutable primitive types (only the ones not covered by v05)
impl<'a> TraceAttributesBoolean for &'a mut bool {
    fn get(&self) -> bool {
        **self
    }

    fn set(self, value: bool) {
        *self = value;
    }
}

impl<'a> TraceAttributesInteger for &'a mut i64 {
    fn get(&self) -> i64 {
        **self
    }

    fn set(self, value: i64) {
        *self = value;
    }
}

// TraceAttributesString implementation for mutable references in v1
impl<'storage, D: TraceDataLifetime<'storage> + 'storage> TraceAttributesString<'storage, 'storage, TracePayload<D>, D> for &'storage mut MutableAttrRef {
    fn get(&self, storage: &'storage TraceStaticData<D>) -> &'storage D::Text {
        storage.get_string(**self)
    }

    fn set(self, storage: &mut TraceStaticData<D>, value: D::Text) {
        *self = storage.add_string(value);
    }
}

// TraceAttributesMutOp for Span - this is the main one we need
impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, Span> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Span>, Span> {
    type MutString = &'a mut MutableAttrRef;
    type MutBytes = ();
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut Span, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| match v {
            AttributeAnyValue::String(s) => {
                // SAFETY: Transmute from 'b to 'a (container to storage lifetime)
                // This is sound because the TraceStringRef index is stable for the storage's lifetime
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }

    fn set(container: &'b mut Span, storage: &mut TraceStaticData<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Span> {
        let key_ref = storage.add_string(key);
        let entry = container.attributes.entry(key_ref).or_insert_with(|| {
            match value {
                AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
                AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
                AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
                AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
                AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
                AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
                AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
            }
        });

        match entry {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut Span, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(r) = storage.find(key) {
            container.attributes.remove(&r);
        }
    }
}

// Similar implementations for other container types
impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, Traces> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Traces>, Traces> {
    type MutString = &'a mut MutableAttrRef;
    type MutBytes = ();
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut Traces, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Traces>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| match v {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }

    fn set(container: &'b mut Traces, storage: &mut TraceStaticData<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Traces> {
        let key_ref = storage.add_string(key);
        let entry = container.attributes.entry(key_ref).or_insert_with(|| {
            match value {
                AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
                AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
                AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
                AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
                AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
                AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
                AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
            }
        });

        match entry {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut Traces, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(r) = storage.find(key) {
            container.attributes.remove(&r);
        }
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, TraceChunk> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, TraceChunk>, TraceChunk> {
    type MutString = &'a mut MutableAttrRef;
    type MutBytes = ();
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut TraceChunk, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, TraceChunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| match v {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }

    fn set(container: &'b mut TraceChunk, storage: &mut TraceStaticData<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, TraceChunk> {
        let key_ref = storage.add_string(key);
        let entry = container.attributes.entry(key_ref).or_insert_with(|| {
            match value {
                AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
                AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
                AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
                AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
                AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
                AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
                AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
            }
        });

        match entry {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut TraceChunk, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(r) = storage.find(key) {
            container.attributes.remove(&r);
        }
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, SpanLink> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanLink>, SpanLink> {
    type MutString = &'a mut MutableAttrRef;
    type MutBytes = ();
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut SpanLink, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanLink>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| match v {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }

    fn set(container: &'b mut SpanLink, storage: &mut TraceStaticData<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanLink> {
        let key_ref = storage.add_string(key);
        let entry = container.attributes.entry(key_ref).or_insert_with(|| {
            match value {
                AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
                AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
                AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
                AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
                AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
                AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
                AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
            }
        });

        match entry {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut SpanLink, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(r) = storage.find(key) {
            container.attributes.remove(&r);
        }
    }
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, SpanEvent> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanEvent>, SpanEvent> {
    type MutString = &'a mut MutableAttrRef;
    type MutBytes = ();
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(container: &'b mut SpanEvent, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanEvent>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| match v {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        })
    }

    fn set(container: &'b mut SpanEvent, storage: &mut TraceStaticData<D>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanEvent> {
        let key_ref = storage.add_string(key);
        let entry = container.attributes.entry(key_ref).or_insert_with(|| {
            match value {
                AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
                AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
                AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
                AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
                AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
                AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
                AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
            }
        });

        match entry {
            AttributeAnyValue::String(s) => {
                let s_storage: &'a mut MutableAttrRef = unsafe { std::mem::transmute(s) };
                AttributeAnyContainer::String(s_storage)
            },
            AttributeAnyValue::Bytes(_) => AttributeAnyContainer::Bytes(()),
            AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
            AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
            AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
            AttributeAnyValue::Array(_) => AttributeAnyContainer::Array(()),
            AttributeAnyValue::Map(_) => AttributeAnyContainer::Map(()),
        }
    }

    fn remove<K>(container: &mut SpanEvent, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(r) = storage.find(key) {
            container.attributes.remove(&r);
        }
    }
}
