mod to_v04;
pub use to_v04::to_v04;

use std::collections::HashMap;
use std::hash::Hash;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{BytesData, SliceData, TraceData, OwnedTraceData, TraceDataLifetime, SpanDataContents, AttributeAnyContainer, AttributeAnySetterContainer, AttrRef, AttrOwned, TraceAttributesMut, TraceAttributesMutOp, TraceAttributesString, TraceAttributesBytes, TraceAttributesInteger, TraceAttributesBoolean, AttributeAnyGetterContainer, AttributeArray, AttributeArrayOp, AttributeArrayMutOp, TraceAttributes, TraceAttributesOp, TraceAttributeGetterTypes, TraceAttributeSetterTypes, TracesMut, Traces as TracesStruct, TraceProjector, AttributeAnyValueType};
use crate::span::table::{TraceBytesRef, TraceDataText, TraceDataBytes, TraceDataRef, TraceStringRef, StaticDataVec};

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

    pub fn add_string(&mut self, value: impl Into<T::Text>) -> TraceStringRef {
        self.strings.add(value.into())
    }

    pub fn add_bytes(&mut self, value: impl Into<T::Bytes>) -> TraceBytesRef {
        self.bytes.add(value.into())
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

// Inline helper: convert an &'a AttributeAnyValue to an AttributeAnyContainer using immutable
// access.
#[inline]
fn attribute_getter<'a, 's, D: TraceDataLifetime<'s>>(
    v: &'a AttributeAnyValue,
    storage: &'s TraceStaticData<D>,
) -> AttributeAnyContainer<&'s D::Text, &'s D::Bytes, bool, i64, f64, &'a Vec<AttributeAnyValue>, &'a HashMap<TraceStringRef, AttributeAnyValue>> {
    match v {
        AttributeAnyValue::String(s) => AttributeAnyContainer::String(storage.get_string(*s)),
        AttributeAnyValue::Bytes(b) => AttributeAnyContainer::Bytes(storage.get_bytes(*b)),
        AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(*b),
        AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(*i),
        AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(*d),
        AttributeAnyValue::Array(arr) => AttributeAnyContainer::Array(arr),
        AttributeAnyValue::Map(map) => AttributeAnyContainer::Map(map),
    }
}

// Inline helper: convert an &'b mut AttributeAnyValue to an AttributeAnyContainer using mutable
// access.  String indices are transmuted from 'b to 'a (the storage/outer lifetime) exactly as
// the top-level Span/TraceChunk/etc. impls do.
#[inline]
#[allow(mutable_transmutes)]
fn v1_to_setter<'b, 'a>(
    v: &'b mut AttributeAnyValue,
) -> AttributeAnyContainer<&'a mut TraceStringRef, &'a mut TraceBytesRef, &'b mut bool, &'b mut i64, &'b mut f64, &'b mut Vec<AttributeAnyValue>, &'b mut HashMap<TraceStringRef, AttributeAnyValue>> {
    match v {
        AttributeAnyValue::String(s) => {
            // SAFETY: same reasoning as in the Span/TraceChunk impls – the TraceStringRef lives
            // in the payload's static storage for 'a; exclusive access is ensured by the
            // mutable borrow chain.
            let s_ref: &'a mut TraceStringRef = unsafe { std::mem::transmute(s) };
            AttributeAnyContainer::String(s_ref)
        },
        AttributeAnyValue::Bytes(b) => {
            // SAFETY: same as String – TraceBytesRef is an index into static storage,
            // valid for 'a; exclusive access is guaranteed by the mutable borrow chain.
            let b_ref: &'a mut TraceBytesRef = unsafe { std::mem::transmute(b) };
            AttributeAnyContainer::Bytes(b_ref)
        },
        AttributeAnyValue::Boolean(b) => AttributeAnyContainer::Boolean(b),
        AttributeAnyValue::Integer(i) => AttributeAnyContainer::Integer(i),
        AttributeAnyValue::Double(d) => AttributeAnyContainer::Double(d),
        AttributeAnyValue::Array(arr) => AttributeAnyContainer::Array(arr),
        AttributeAnyValue::Map(map) => AttributeAnyContainer::Map(map),
    }
}

// Helper: create a fresh AttributeAnyValue of the requested kind for append/set operations.
#[inline]
fn new_v1_value(kind: AttributeAnyValueType) -> AttributeAnyValue {
    match kind {
        AttributeAnyValueType::String => AttributeAnyValue::String(TraceDataRef::default()),
        AttributeAnyValueType::Bytes => AttributeAnyValue::Bytes(TraceDataRef::default()),
        AttributeAnyValueType::Boolean => AttributeAnyValue::Boolean(false),
        AttributeAnyValueType::Integer => AttributeAnyValue::Integer(0),
        AttributeAnyValueType::Double => AttributeAnyValue::Double(0.0),
        AttributeAnyValueType::Array => AttributeAnyValue::Array(Vec::new()),
        AttributeAnyValueType::Map => AttributeAnyValue::Map(HashMap::new()),
    }
}

// ── TraceAttributeGetterTypes for &'a Vec / &'a mut Vec ──────────────────────
// Arrays are index-based; no key-based (TraceAttributesOp) impl is needed or sensible.

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    TraceAttributeGetterTypes<'container, 'storage, TracePayload<D>, D, &'container Vec<AttributeAnyValue>>
    for &'container Vec<AttributeAnyValue>
{
    type Array = &'container Vec<AttributeAnyValue>;
    type Map = &'container HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    TraceAttributeGetterTypes<'container, 'storage, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>
    for &'container mut Vec<AttributeAnyValue>
{
    type Array = &'container Vec<AttributeAnyValue>;
    type Map = &'container HashMap<TraceStringRef, AttributeAnyValue>;
}

// ── TraceAttributeGetterTypes for &'a HashMap / &'a mut HashMap ──────────────

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    TraceAttributeGetterTypes<'container, 'storage, TracePayload<D>, D, &'container HashMap<TraceStringRef, AttributeAnyValue>>
    for &'container HashMap<TraceStringRef, AttributeAnyValue>
{
    type Array = &'container Vec<AttributeAnyValue>;
    type Map = &'container HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    TraceAttributeGetterTypes<'container, 'storage, TracePayload<D>, D, &'container mut HashMap<TraceStringRef, AttributeAnyValue>>
    for &'container mut HashMap<TraceStringRef, AttributeAnyValue>
{
    type Array = &'container Vec<AttributeAnyValue>;
    type Map = &'container HashMap<TraceStringRef, AttributeAnyValue>;
}

// ── TraceAttributeGetterTypes for TraceAttributes with AttrOwned<&HashMap> ────
// Required as supertrait of TraceAttributesOp<..., &HashMap> for TraceAttributes<..., AttrOwned<&HashMap>, ...>.

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a HashMap<TraceStringRef, AttributeAnyValue>>, &'a HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, &'a mut HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a mut HashMap<TraceStringRef, AttributeAnyValue>>, &'a mut HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, &'a mut Vec<AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a mut Vec<AttributeAnyValue>>, &'a mut Vec<AttributeAnyValue>, ISMUT>
{
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

// ── TraceAttributesOp for &'a HashMap<TraceStringRef, AttributeAnyValue> ──────

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributesOp<'a, 's, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a HashMap<TraceStringRef, AttributeAnyValue>>, &'a HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    fn get<K>(
        container: &'a &'a HashMap<TraceStringRef, AttributeAnyValue>,
        storage: &'s TraceStaticData<D>,
        key: &K,
    ) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        (*container).get(&r).map(|v| attribute_getter(v, storage))
    }
}

// ── TraceAttributeGetterTypes and TraceAttributesOp for AttrRef<'container, &'a HashMap> ──
// Required for key-based reads on nested map values returned by get_map().
// When get_map() returns TraceAttributes<'s, T, D, AttrOwned<&'a HashMap>, &'a HashMap>,
// calling get_string() on it internally dispatches via AttrRef<'container, &'a HashMap>
// (where 'a: 'container) to look up values in the nested map.

impl<'container, 'a: 'container, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributeGetterTypes<'container, 's, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'container, &'a HashMap<TraceStringRef, AttributeAnyValue>>, &'a HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'container, 'a: 'container, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributesOp<'container, 's, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'container, &'a HashMap<TraceStringRef, AttributeAnyValue>>, &'a HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    fn get<K>(
        container: &'container &'a HashMap<TraceStringRef, AttributeAnyValue>,
        storage: &'s TraceStaticData<D>,
        key: &K,
    ) -> Option<AttributeAnyGetterContainer<'container, 's, Self, TracePayload<D>, D, &'a HashMap<TraceStringRef, AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        (*container).get(&r).map(|v| attribute_getter(v, storage))
    }
}

// ── TraceAttributesOp for &'a mut HashMap<TraceStringRef, AttributeAnyValue> ──
// Needed as the supertrait of TraceAttributesMutOp for &'b mut HashMap.

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributesOp<'a, 's, TracePayload<D>, D, &'a mut HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a mut HashMap<TraceStringRef, AttributeAnyValue>>, &'a mut HashMap<TraceStringRef, AttributeAnyValue>, ISMUT>
{
    fn get<K>(
        container: &'a &'a mut HashMap<TraceStringRef, AttributeAnyValue>,
        storage: &'s TraceStaticData<D>,
        key: &K,
    ) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, &'a mut HashMap<TraceStringRef, AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        // .get() defaults to the trait method instead of the HashMap impl otherwise
        let map: &HashMap<TraceStringRef, AttributeAnyValue> = *container;
        map.get(&r).map(|v| attribute_getter(v, storage))
    }
}

// ── TraceAttributesOp for &'a mut Vec<AttributeAnyValue> ─────────────────────
// Arrays don't support key-based access; needed as supertrait of TraceAttributesMutOp for &'b mut Vec.

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8>
    TraceAttributesOp<'a, 's, TracePayload<D>, D, &'a mut Vec<AttributeAnyValue>>
    for TraceAttributes<'s, TracePayload<D>, D, AttrOwned<&'a mut Vec<AttributeAnyValue>>, &'a mut Vec<AttributeAnyValue>, ISMUT>
{
    fn get<K>(
        _container: &'a &'a mut Vec<AttributeAnyValue>,
        _storage: &'s TraceStaticData<D>,
        _key: &K,
    ) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, &'a mut Vec<AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }
}

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    AttributeArrayOp<'container, 'storage, TracePayload<D>, D>
    for &'container Vec<AttributeAnyValue>
{
    fn get_attribute_array_len(&self, _storage: &'storage TraceStaticData<D>) -> usize {
        self.len()
    }

    fn get_attribute_array_value(
        &'container self,
        storage: &'storage TraceStaticData<D>,
        index: usize,
    ) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, TracePayload<D>, D, &'container Vec<AttributeAnyValue>>, TracePayload<D>, D, &'container Vec<AttributeAnyValue>>
    {
        attribute_getter(&(*self)[index], storage)
    }
}

impl<'container, 'storage, D: TraceDataLifetime<'storage> + 'storage>
    AttributeArrayOp<'container, 'storage, TracePayload<D>, D>
    for &'container mut Vec<AttributeAnyValue>
{
    fn get_attribute_array_len(&self, _storage: &'storage TraceStaticData<D>) -> usize {
        self.len()
    }

    fn get_attribute_array_value(
        &'container self,
        storage: &'storage TraceStaticData<D>,
        index: usize,
    ) -> AttributeAnyGetterContainer<'container, 'storage, AttributeArray<'container, 'storage, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>
    {
        attribute_getter(&(*self)[index], storage)
    }
}

impl<'container, 'storage, D: TraceData + TraceDataLifetime<'storage> + 'storage>
    AttributeArrayMutOp<'container, 'storage, TracePayload<D>, D>
    for &'container mut Vec<AttributeAnyValue>
{
    fn get_attribute_array_value_mut(
        &'container mut self,
        _storage: &mut TraceStaticData<D>,
        index: usize,
    ) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, TracePayload<D>, D, AttrOwned<&'container mut Vec<AttributeAnyValue>>, &'container mut Vec<AttributeAnyValue>>, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>>
    {
        let item = (*self).get_mut(index)?;
        Some(v1_to_setter(item))
    }

    fn set(
        &'container mut self,
        _storage: &mut TraceStaticData<D>,
        index: usize,
        value: AttributeAnyValueType,
    ) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, TracePayload<D>, D, AttrOwned<&'container mut Vec<AttributeAnyValue>>, &'container mut Vec<AttributeAnyValue>>, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>
    {
        let vec: &'container mut Vec<AttributeAnyValue> = *self;
        vec[index] = new_v1_value(value);
        v1_to_setter(vec.get_mut(index).unwrap())
    }

    fn append_attribute_array_value(
        &'container mut self,
        _storage: &mut TraceStaticData<D>,
        value: AttributeAnyValueType,
    ) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, TracePayload<D>, D, AttrOwned<&'container mut Vec<AttributeAnyValue>>, &'container mut Vec<AttributeAnyValue>>, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>
    {
        let vec: &'container mut Vec<AttributeAnyValue> = *self;
        vec.push(new_v1_value(value));
        v1_to_setter(vec.last_mut().unwrap())
    }

    fn swap_attribute_array_values(
        &mut self,
        _storage: &mut TraceStaticData<D>,
        i: usize,
        j: usize,
    ) {
        self.swap(i, j);
    }

    fn truncate_attribute_array_values(
        &mut self,
        _storage: &mut TraceStaticData<D>,
        len: usize,
    ) {
        self.truncate(len);
    }
}

// ── TraceAttributeSetterTypes for &'container mut Vec<AttributeAnyValue> ──────
// Direct impl required by AttributeArrayMutOp supertrait (TraceAttributeSetterTypes<..., Self>).

impl<'container, 'storage, D: TraceData + TraceDataLifetime<'storage> + 'storage>
    TraceAttributeSetterTypes<'container, 'storage, TracePayload<D>, D, &'container mut Vec<AttributeAnyValue>>
    for &'container mut Vec<AttributeAnyValue>
{
    type MutString = &'storage mut TraceStringRef;
    type MutBytes = &'storage mut TraceBytesRef;
    type MutBoolean = &'container mut bool;
    type MutInteger = &'container mut i64;
    type MutDouble = &'container mut f64;
    type MutArray = &'container mut Vec<AttributeAnyValue>;
    type MutMap = &'container mut HashMap<TraceStringRef, AttributeAnyValue>;
}

// ── TraceAttributesMutOp for &'b mut Vec<AttributeAnyValue> ──────────────────
// Arrays don't support key-based mutation; all methods are no-ops / return None.

impl<'a, 'b, D: TraceData>
    TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, &'b mut Vec<AttributeAnyValue>>
    for TraceAttributesMut<'a, TracePayload<D>, D, AttrOwned<&'b mut Vec<AttributeAnyValue>>, &'b mut Vec<AttributeAnyValue>>
{
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData>
    TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, &'b mut Vec<AttributeAnyValue>>
    for TraceAttributesMut<'a, TracePayload<D>, D, AttrOwned<&'b mut Vec<AttributeAnyValue>>, &'b mut Vec<AttributeAnyValue>>
{
    fn get_mut<K>(
        _container: &'b mut &'b mut Vec<AttributeAnyValue>,
        _storage: &mut TraceStaticData<D>,
        _key: &K,
    ) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, &'b mut Vec<AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }

    fn set(
        _container: &'b mut &'b mut Vec<AttributeAnyValue>,
        _storage: &mut TraceStaticData<D>,
        _key: D::Text,
        _value: AttributeAnyValueType,
    ) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, &'b mut Vec<AttributeAnyValue>> {
        unreachable!("V1 arrays do not support key-based attribute insertion")
    }

    fn remove<K>(_container: &mut &'b mut Vec<AttributeAnyValue>, _storage: &mut TraceStaticData<D>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {}
}

impl<'a, 'b, D: TraceData>
    TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, &'b mut HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributesMut<'a, TracePayload<D>, D, AttrOwned<&'b mut HashMap<TraceStringRef, AttributeAnyValue>>, &'b mut HashMap<TraceStringRef, AttributeAnyValue>>
{
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData>
    TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, &'b mut HashMap<TraceStringRef, AttributeAnyValue>>
    for TraceAttributesMut<'a, TracePayload<D>, D, AttrOwned<&'b mut HashMap<TraceStringRef, AttributeAnyValue>>, &'b mut HashMap<TraceStringRef, AttributeAnyValue>>
{
    fn get_mut<K>(
        container: &'b mut &'b mut HashMap<TraceStringRef, AttributeAnyValue>,
        storage: &mut TraceStaticData<D>,
        key: &K,
    ) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, &'b mut HashMap<TraceStringRef, AttributeAnyValue>>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        (*container).get_mut(&r).map(|v| v1_to_setter(v))
    }

    fn set(
        container: &'b mut &'b mut HashMap<TraceStringRef, AttributeAnyValue>,
        storage: &mut TraceStaticData<D>,
        key: D::Text,
        value: AttributeAnyValueType,
    ) -> AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, &'b mut HashMap<TraceStringRef, AttributeAnyValue>> {
        let key_ref = storage.add_string(key);
        let entry = (*container).entry(key_ref).or_insert_with(|| new_v1_value(value));
        v1_to_setter(entry)
    }

    fn remove<K>(container: &mut &'b mut HashMap<TraceStringRef, AttributeAnyValue>, storage: &mut TraceStaticData<D>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        if let Some(r) = storage.find(key) {
            (*container).remove(&r);
        }
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
impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, Traces> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Traces>, Traces, ISMUT> {
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, Traces> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Traces>, Traces, ISMUT> {
    fn get<K>(container: &'a Traces, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, Traces>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| attribute_getter(v, storage))
    }
}

// Similar implementations for TraceChunk, Span, SpanLink, SpanEvent
impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, TraceChunk> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, TraceChunk>, TraceChunk, ISMUT> {
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, TraceChunk> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, TraceChunk>, TraceChunk, ISMUT> {
    fn get<K>(container: &'a TraceChunk, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, TraceChunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| attribute_getter(v, storage))
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, Span> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Span>, Span, ISMUT> {
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, Span> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, Span>, Span, ISMUT> {
    fn get<K>(container: &'a Span, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| attribute_getter(v, storage))
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, SpanLink> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanLink>, SpanLink, ISMUT> {
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, SpanLink> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanLink>, SpanLink, ISMUT> {
    fn get<K>(container: &'a SpanLink, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, SpanLink>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| attribute_getter(v, storage))
    }
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributeGetterTypes<'a, 's, TracePayload<D>, D, SpanEvent> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanEvent>, SpanEvent, ISMUT> {
    type Array = &'a Vec<AttributeAnyValue>;
    type Map = &'a HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 's, D: TraceDataLifetime<'s> + 's, const ISMUT: u8> TraceAttributesOp<'a, 's, TracePayload<D>, D, SpanEvent> for TraceAttributes<'s, TracePayload<D>, D, AttrRef<'a, SpanEvent>, SpanEvent, ISMUT> {
    fn get<K>(container: &'a SpanEvent, storage: &'s TraceStaticData<D>, key: &K) -> Option<AttributeAnyGetterContainer<'a, 's, Self, TracePayload<D>, D, SpanEvent>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let r = storage.find(key)?;
        container.attributes.get(&r).map(|v| attribute_getter(v, storage))
    }
}

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
impl<'storage, D: TraceDataLifetime<'storage> + 'storage> TraceAttributesString<'storage, 'storage, TracePayload<D>, D> for &'storage mut TraceStringRef {
    fn get(&self, storage: &'storage TraceStaticData<D>) -> &'storage D::Text {
        storage.get_string(**self)
    }

    fn set(self, storage: &mut TraceStaticData<D>, value: D::Text) {
        *self = storage.add_string(value);
    }
}

// TraceAttributesBytes implementation for mutable references in v1
impl<'storage, D: TraceDataLifetime<'storage> + 'storage> TraceAttributesBytes<'storage, 'storage, TracePayload<D>, D> for &'storage mut TraceBytesRef {
    fn get(&self, storage: &'storage TraceStaticData<D>) -> &'storage D::Bytes {
        storage.get_bytes(**self)
    }

    fn set(self, storage: &mut TraceStaticData<D>, value: D::Bytes) {
        *self = storage.add_bytes(value);
    }
}

// TraceAttributesMutOp for Span - this is the main one we need
impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, Span> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Span>, Span> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, Span> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Span>, Span> {
    fn get_mut<K>(container: &'b mut Span, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Span>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| v1_to_setter(v))
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

        v1_to_setter(entry)
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
impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, Traces> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Traces>, Traces> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, Traces> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, Traces>, Traces> {
    fn get_mut<K>(container: &'b mut Traces, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, Traces>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| v1_to_setter(v))
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

        v1_to_setter(entry)
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

impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, TraceChunk> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, TraceChunk>, TraceChunk> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, TraceChunk> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, TraceChunk>, TraceChunk> {
    fn get_mut<K>(container: &'b mut TraceChunk, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, TraceChunk>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| v1_to_setter(v))
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

        v1_to_setter(entry)
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

impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, SpanLink> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanLink>, SpanLink> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, SpanLink> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanLink>, SpanLink> {
    fn get_mut<K>(container: &'b mut SpanLink, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanLink>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| v1_to_setter(v))
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

        v1_to_setter(entry)
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

impl<'a, 'b, D: TraceData> TraceAttributeSetterTypes<'b, 'a, TracePayload<D>, D, SpanEvent> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanEvent>, SpanEvent> {
    type MutString = &'a mut TraceStringRef;
    type MutBytes = &'a mut TraceBytesRef;
    type MutBoolean = &'b mut bool;
    type MutInteger = &'b mut i64;
    type MutDouble = &'b mut f64;
    type MutArray = &'b mut Vec<AttributeAnyValue>;
    type MutMap = &'b mut HashMap<TraceStringRef, AttributeAnyValue>;
}

impl<'a, 'b, D: TraceData> TraceAttributesMutOp<'b, 'a, TracePayload<D>, D, SpanEvent> for TraceAttributesMut<'a, TracePayload<D>, D, AttrRef<'b, SpanEvent>, SpanEvent> {
    fn get_mut<K>(container: &'b mut SpanEvent, storage: &mut TraceStaticData<D>, key: &K) -> Option<AttributeAnySetterContainer<'b, 'a, Self, TracePayload<D>, D, SpanEvent>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        let r = storage.find(key)?;
        container.attributes.get_mut(&r).map(|v| v1_to_setter(v))
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

        v1_to_setter(entry)
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

#[cfg(test)]
mod tests {
    use super::{AttributeAnyValue, Span, SpanEvent, TraceChunk, TracePayload};
    use crate::span::{BytesData, TraceProjector};
    use libdd_tinybytes::BytesString;
    use std::collections::HashMap;

    #[test]
    fn test_basic_span_event_attributes() {
        let mut payload = TracePayload::<BytesData>::default();

        let str_key = payload.static_data.add_string("str_attr");
        let bool_key = payload.static_data.add_string("bool_attr");
        let int_key = payload.static_data.add_string("int_attr");
        let double_key = payload.static_data.add_string("double_attr");
        let event_name = payload.static_data.add_string("test_event");
        let str_val = payload.static_data.add_string("hello");

        payload.traces.chunks.push(TraceChunk {
            spans: vec![Span {
                span_events: vec![SpanEvent {
                    time_unix_nano: 12345,
                    name: event_name,
                    attributes: HashMap::from([
                        (str_key, AttributeAnyValue::String(str_val)),
                        (bool_key, AttributeAnyValue::Boolean(true)),
                        (int_key, AttributeAnyValue::Integer(42)),
                        (double_key, AttributeAnyValue::Double(3.14)),
                    ]),
                }],
                ..Default::default()
            }],
            ..Default::default()
        });

        // Test reading attributes
        {
            let traces = payload.project();
            for chunk in traces.chunks() {
                for span in chunk.spans() {
                    for event in span.span_events() {
                        assert_eq!(event.attributes().get_string("str_attr"), Some(&BytesString::from("hello")));
                        assert_eq!(event.attributes().get_bool("bool_attr"), Some(true));
                        assert_eq!(event.attributes().get_int("int_attr"), Some(42));
                        assert_eq!(event.attributes().get_double("double_attr"), Some(3.14));
                        assert_eq!(event.attributes().get_string("nonexistent"), None);
                    }
                }
            }
        }

        // Test writing attributes
        {
            let mut traces_mut = payload.project_mut();
            for mut chunk in traces_mut.chunks_mut() {
                for mut span in chunk.spans_mut() {
                    for mut event in span.span_events_mut() {
                        event.attributes_mut().set_string("str_attr", "world");
                        event.attributes_mut().set_int("new_int", 100);
                    }
                }
            }
        }

        // Verify modifications
        {
            let traces = payload.project();
            for chunk in traces.chunks() {
                for span in chunk.spans() {
                    for event in span.span_events() {
                        assert_eq!(event.attributes().get_string("str_attr"), Some(&BytesString::from("world")));
                        assert_eq!(event.attributes().get_int("new_int"), Some(100));
                    }
                }
            }
        }
    }

    #[test]
    fn test_array_attribute_projection() {
        let mut payload = TracePayload::<BytesData>::default();

        let array_key = payload.static_data.add_string("array_attr");
        let event_name = payload.static_data.add_string("event");
        let str_val = payload.static_data.add_string("item_str");

        payload.traces.chunks.push(TraceChunk {
            spans: vec![Span {
                span_events: vec![SpanEvent {
                    time_unix_nano: 0,
                    name: event_name,
                    attributes: HashMap::from([(
                        array_key,
                        AttributeAnyValue::Array(vec![
                            AttributeAnyValue::Integer(10),
                            AttributeAnyValue::Integer(20),
                            AttributeAnyValue::String(str_val),
                            AttributeAnyValue::Boolean(true),
                        ]),
                    )]),
                }],
                ..Default::default()
            }],
            ..Default::default()
        });

        let traces = payload.project();
        for chunk in traces.chunks() {
            for span in chunk.spans() {
                for event in span.span_events() {
                    let arr = event.attributes().get_array("array_attr")
                        .expect("array_attr should exist");
                    assert_eq!(arr.len(), 4);
                    assert_eq!(arr.get_int(0), Some(10));
                    assert_eq!(arr.get_int(1), Some(20));
                    assert_eq!(arr.get_string(2), Some(&BytesString::from("item_str")));
                    assert_eq!(arr.get_bool(3), Some(true));
                    assert_eq!(arr.get_int(4), None); // out of bounds
                }
            }
        }
    }

    #[test]
    fn test_nested_map_projection() {
        let mut payload = TracePayload::<BytesData>::default();

        let map_key = payload.static_data.add_string("map_attr");
        let event_name = payload.static_data.add_string("event");
        let nested_str_key = payload.static_data.add_string("nested_str");
        let nested_int_key = payload.static_data.add_string("nested_int");
        let nested_str_val = payload.static_data.add_string("nested_value");

        payload.traces.chunks.push(TraceChunk {
            spans: vec![Span {
                span_events: vec![SpanEvent {
                    time_unix_nano: 0,
                    name: event_name,
                    attributes: HashMap::from([(
                        map_key,
                        AttributeAnyValue::Map(HashMap::from([
                            (nested_str_key, AttributeAnyValue::String(nested_str_val)),
                            (nested_int_key, AttributeAnyValue::Integer(99)),
                        ])),
                    )]),
                }],
                ..Default::default()
            }],
            ..Default::default()
        });

        let traces = payload.project();
        for chunk in traces.chunks() {
            for span in chunk.spans() {
                for event in span.span_events() {
                    let nested = event.attributes().get_map("map_attr")
                        .expect("map_attr should exist");
                    assert_eq!(nested.get_string("nested_str"), Some(&BytesString::from("nested_value")));
                    assert_eq!(nested.get_int("nested_int"), Some(99));
                    assert_eq!(nested.get_string("nonexistent"), None);
                }
            }
        }
    }

    #[test]
    fn test_array_retain_mut() {
        use crate::span::AttributeAnyContainer;

        let mut payload = TracePayload::<BytesData>::default();

        let array_key = payload.static_data.add_string("array_attr");
        let event_name = payload.static_data.add_string("event");

        payload.traces.chunks.push(TraceChunk {
            spans: vec![Span {
                span_events: vec![SpanEvent {
                    time_unix_nano: 0,
                    name: event_name,
                    attributes: HashMap::from([(
                        array_key,
                        AttributeAnyValue::Array(vec![
                            AttributeAnyValue::Integer(10),
                            AttributeAnyValue::Integer(20),
                            AttributeAnyValue::Integer(5),
                            AttributeAnyValue::Integer(30),
                            AttributeAnyValue::Integer(1),
                        ]),
                    )]),
                }],
                ..Default::default()
            }],
            ..Default::default()
        });

        // Retain only integers > 10
        {
            let mut traces_mut = payload.project_mut();
            for mut chunk in traces_mut.chunks_mut() {
                for mut span in chunk.spans_mut() {
                    for mut event in span.span_events_mut() {
                        let mut attrs = event.attributes_mut();
                        let mut arr = attrs.get_array_mut("array_attr")
                            .expect("array_attr should exist");
                        arr.retain_mut(|elem| {
                            if let AttributeAnyContainer::Integer(i) = elem {
                                *i > 10
                            } else {
                                false
                            }
                        });
                    }
                }
            }
        }

        // Verify that only [20, 30] remain (in order)
        {
            let traces = payload.project();
            for chunk in traces.chunks() {
                for span in chunk.spans() {
                    for event in span.span_events() {
                        let arr = event.attributes().get_array("array_attr")
                            .expect("array_attr should exist");
                        assert_eq!(arr.len(), 2);
                        assert_eq!(arr.get_int(0), Some(20));
                        assert_eq!(arr.get_int(1), Some(30));
                    }
                }
            }
        }
    }

    #[test]
    fn test_retain_span_events() {
        let mut payload = TracePayload::<BytesData>::default();

        let keep_a = payload.static_data.add_string("keep_a");
        let discard_b = payload.static_data.add_string("discard_b");
        let keep_c = payload.static_data.add_string("keep_c");

        payload.traces.chunks.push(TraceChunk {
            spans: vec![Span {
                span_events: vec![
                    SpanEvent { time_unix_nano: 1, name: keep_a, attributes: HashMap::new() },
                    SpanEvent { time_unix_nano: 2, name: discard_b, attributes: HashMap::new() },
                    SpanEvent { time_unix_nano: 3, name: keep_c, attributes: HashMap::new() },
                ],
                ..Default::default()
            }],
            ..Default::default()
        });

        // Drop the middle event. SpanEvent is invariant in 's (due to T: TraceProjector<'s, D>),
        // which prevents calling getter methods inside a for<'r> HRTB closure. Use a position
        // counter instead.
        {
            let mut traces_mut = payload.project_mut();
            for mut chunk in traces_mut.chunks_mut() {
                for mut span in chunk.spans_mut() {
                    let mut idx = 0usize;
                    span.retain_span_events(|_event| {
                        idx += 1;
                        idx != 2 // drop the 2nd event
                    });
                }
            }
        }

        // Verify "keep_a" (time 1) and "keep_c" (time 3) remain in order
        {
            let traces = payload.project();
            for chunk in traces.chunks() {
                for span in chunk.spans() {
                    let events: Vec<_> = span.span_events().collect();
                    assert_eq!(events.len(), 2);
                    assert_eq!(events[0].time_unix_nano(), 1);
                    assert_eq!(events[0].name(), &BytesString::from("keep_a"));
                    assert_eq!(events[1].time_unix_nano(), 3);
                    assert_eq!(events[1].name(), &BytesString::from("keep_c"));
                }
            }
        }
    }
}
