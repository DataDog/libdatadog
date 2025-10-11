use std::marker::PhantomData;
use datadog_trace_protobuf::pb::idx::SpanKind;
use crate::span::{SpanBytes, SpanText, TraceData};

pub trait TraceProjectorDependencies<D: TraceData, Projector: TraceProjector<D>> {
    type AttributeTrace<'a>: TraceAttributesOp<Projector, D, Projector::Trace<'a>> + 'a;
    type AttributeChunk<'a>: TraceAttributesOp<Projector, D, Projector::Chunk<'a>> + 'a;
    type AttributeSpan<'a>: TraceAttributesOp<Projector, D, Projector::Span<'a>> + 'a;
    type AttributeSpanLink<'a>: TraceAttributesOp<Projector, D, Projector::SpanLink<'a>> + 'a;
    type AttributeSpanEvent<'a>: TraceAttributesOp<Projector, D, Projector::SpanEvent<'a>> + 'a;
}

pub trait TraceProjector<D: TraceData>: Sized + for<'a> TraceProjectorDependencies<
    D,
    Self,
    AttributeTrace<'a> = TraceAttributes<'a, Self, D, AttrRef<'a, Self::Trace<'a>>, Self::Trace<'a>>,
    AttributeChunk<'a> = TraceAttributes<'a, Self, D, AttrRef<'a, Self::Chunk<'a>>, Self::Chunk<'a>>,
    AttributeSpan<'a> = TraceAttributes<'a, Self, D, AttrRef<'a, Self::Span<'a>>, Self::Span<'a>>,
    AttributeSpanLink<'a> = TraceAttributes<'a, Self, D, AttrRef<'a, Self::SpanLink<'a>>, Self::SpanLink<'a>>,
    AttributeSpanEvent<'a> = TraceAttributes<'a, Self, D, AttrRef<'a, Self::SpanEvent<'a>>, Self::SpanEvent<'a>>,
>
{
    type Storage<'a>: 'a;
    type Trace<'a>: 'a;
    type Chunk<'a>: 'a;
    type Span<'a>: 'a;
    type SpanLink<'a>: 'a;
    type SpanEvent<'a>: 'a;

    fn project(&self) -> Traces<Self, D>;
    fn project_mut(&mut self) -> TracesMut<Self, D>;

    fn add_chunk<'a>(trace: &'a mut Self::Trace<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::Chunk<'a>;
    fn chunk_iterator(trace: &Self::Trace<'_>) -> std::slice::Iter<Self::Chunk<'_>>;
    fn retain_chunks(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, predicate: impl FnMut(&mut Self::Chunk<'_>, &mut Self::Storage<'_>) -> bool);
    fn add_span<'a>(chunk: &'a mut Self::Chunk<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::Span<'a>;
    fn span_iterator(chunk: &Self::Chunk<'_>) -> std::slice::Iter<Self::Span<'_>>;
    fn retain_spans(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, predicate: impl FnMut(&mut Self::Span<'_>, &mut Self::Storage<'_>) -> bool);
    fn add_span_link<'a>(span: &'a mut Self::Span<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::SpanLink<'a>;
    fn span_link_iterator(span: &Self::Span<'_>) -> std::slice::Iter<Self::SpanLink<'_>>;
    fn retain_span_links(chunk: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, predicate: impl FnMut(&mut Self::SpanLink<'_>, &mut Self::Storage<'_>) -> bool);
    fn add_span_event<'a>(span: &mut Self::Span<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::SpanEvent<'a>;
    fn span_event_iterator(span: &Self::Span<'_>) -> std::slice::Iter<Self::SpanEvent<'_>>;
    fn retain_span_events(chunk: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, predicate: impl FnMut(&mut Self::SpanEvent<'_>, &mut Self::Storage<'_>) -> bool);

    fn get_trace_container_id<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_language_name<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_language_version<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_tracer_version<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_runtime_id<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_env<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_hostname<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_app_version<'a>(trace: &'a Self::Trace<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;

    fn set_trace_container_id(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_language_name(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_language_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_tracer_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_runtime_id(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_env(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_hostname(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_app_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);

    fn get_chunk_priority(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> i32;
    fn get_chunk_origin<'a>(chunk: &'a Self::Chunk<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_chunk_dropped_trace(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> bool;
    fn get_chunk_trace_id(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> u128;
    fn get_chunk_sampling_mechanism(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> u32;

    fn set_chunk_priority(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: i32);
    fn set_chunk_origin(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_chunk_dropped_trace(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: bool);
    fn set_chunk_trace_id(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: u128);
    fn set_chunk_sampling_mechanism(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: u32);

    fn get_span_service<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_name<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_resource<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_type<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_span_id(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_span_parent_id(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_span_start(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> i64;
    fn get_span_duration(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> i64;
    fn get_span_error(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> bool;
    fn get_span_env<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_version<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_component<'a>(span: &'a Self::Span<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_kind(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> SpanKind;

    fn set_span_service(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_name(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_resource(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_type(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_span_id(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_span_parent_id(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_span_start(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: i64);
    fn set_span_duration(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: i64);
    fn set_span_error(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: bool);
    fn set_span_env(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_version(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_component(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_span_kind(span: &mut Self::Span<'_>, storage: &mut Self::Storage<'_>, value: SpanKind);

    fn get_link_trace_id(link: &Self::SpanLink<'_>, storage: &Self::Storage<'_>) -> u128;
    fn get_link_span_id(link: &Self::SpanLink<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_link_trace_state<'a>(link: &'a Self::SpanLink<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_link_flags(link: &Self::SpanLink<'_>, storage: &Self::Storage<'_>) -> u32;

    fn set_link_trace_id(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u128);
    fn set_link_span_id(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_link_trace_state(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_link_flags(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u32);

    fn get_event_time_unix_nano(event: &Self::SpanEvent<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_event_name<'a>(event: &'a Self::SpanEvent<'a>, storage: &'a Self::Storage<'a>) -> &'a D::Text;

    fn set_event_time_unix_nano(event: &mut Self::SpanEvent<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_event_name(event: &mut Self::SpanEvent<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
}

const IMMUT: u8 = 0;
const MUT: u8 = 1;

unsafe fn as_mut<T>(v: &T) -> &mut T {
    &mut *(v as *const _ as *mut _)
}

struct TraceValue<'a, T: TraceProjector<D>, D: TraceData, C, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: &'a C,
}

pub type TracesValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> = TraceValue<'a, T, D, T::Trace<'a>, Type, Mut>;
pub type ChunkValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> = TraceValue<'a, T, D, T::Chunk<'a>, Type, Mut>;
pub type SpanValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> = TraceValue<'a, T, D, T::Span<'a>, Type, Mut>;
pub type SpanLinkValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> = TraceValue<'a, T, D, T::SpanLink<'a>, Type, Mut>;
pub type SpanEventValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> = TraceValue<'a, T, D, T::SpanEvent<'a>, Type, Mut>;

#[derive(Debug)]
pub struct Traces<'a, T: TraceProjector<D> + ?Sized, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    traces: &'a T::Trace<'a>,
}
pub type TracesMut<'a, T, D> = Traces<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData> Clone for Traces<'_, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Traces {
            storage: self.storage,
            traces: self.traces,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData> Copy for Traces<'_, T, D> {}

impl<'a, T: TraceProjector<D>, D: TraceData> Traces<'a, T, D> {
    pub fn new(traces: &'a T::Trace<'a>, storage: &'a T::Storage<'a>) -> Self {
        Self::generic_new(traces, storage)
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8> Traces<'a, T, D, Mut> {
    fn generic_new(traces: &'a T::Trace<'a>, storage: &'a T::Storage<'a>) -> Self {
        Traces {
            storage,
            traces,
        }
    }

    pub fn container_id(&self) -> &'a D::Text {
        T::get_trace_container_id(self.traces, self.storage)
    }

    pub fn language_name(&self) -> &'a D::Text {
        T::get_trace_language_name(self.traces, self.storage)
    }

    pub fn language_version(&self) -> &'a D::Text {
        T::get_trace_language_version(self.traces, self.storage)
    }

    pub fn tracer_version(&self) -> &'a D::Text {
        T::get_trace_tracer_version(self.traces, self.storage)
    }

    pub fn runtime_id(&self) -> &'a D::Text {
        T::get_trace_runtime_id(self.traces, self.storage)
    }

    pub fn env(&self) -> &'a D::Text {
        T::get_trace_env(self.traces, self.storage)
    }

    pub fn hostname(&self) -> &'a D::Text {
        T::get_trace_hostname(self.traces, self.storage)
    }

    pub fn app_version(&self) -> &'a D::Text {
        T::get_trace_app_version(self.traces, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::Trace<'a>>, T::Trace<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.traces),
            _phantom: PhantomData,
        }
    }

    pub fn chunks(&self) -> ChunkIterator<'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> TracesMut<'a, T, D> {
    pub fn new_mut(traces: &'a mut T::Trace<'a>, storage: &'a mut T::Storage<'a>) -> Self {
        Self::generic_new(traces, storage)
    }

    pub fn set_container_id<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_container_id(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_language_name<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_language_name(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_language_version<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_language_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_tracer_version<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_tracer_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_runtime_id<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_runtime_id(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_env<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_env(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_hostname<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_hostname(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_app_version<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_app_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'a, T, D, AttrRef<'a, T::Trace<'a>>, T::Trace<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.traces),
            _phantom: PhantomData,
        }
    }

    pub fn chunks_mut(&mut self) -> ChunkIteratorMut<'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }

    pub fn retain_chunks(&mut self, mut predicate: impl FnMut(&mut TraceChunkMut<T, D>) -> bool) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            T::retain_chunks(as_mut(self.traces), as_mut(self.storage), |chunk, storage|
                predicate(&mut TraceChunk {
                    storage,
                    chunk,
                })
            )
        }
    }

    pub fn add_chunk(&mut self) -> TraceChunkMut<T, D> {
        TraceChunk {
            storage: self.storage,
            chunk: unsafe { T::add_chunk(as_mut(self.traces), as_mut(self.storage)) },
        }
    }
}

pub struct ChunkIterator<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Chunk<'a>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type ChunkIteratorMut<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Chunk<'a>>> = ChunkIterator<'a, T, D, I, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Chunk<'a>>, const Mut: u8> Iterator for ChunkIterator<'a, T, D, I, Mut> {
    type Item = TraceChunk<'a, T, D, Mut>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|chunk| {
            TraceChunk {
                storage: self.storage,
                chunk,
            }
        })
    }
}

#[derive(Debug)]
pub struct TraceChunk<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    chunk: &'a T::Chunk<'a>,
}
pub type TraceChunkMut<'a, T, D> = TraceChunk<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData> Clone for TraceChunk<'_, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceChunk {
            storage: self.storage,
            chunk: self.chunk,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData> Copy for TraceChunk<'_, T, D> {}

impl<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8> TraceChunk<'a, T, D, Mut> {
    pub fn priority(&self) -> i32 {
        T::get_chunk_priority(self.chunk, self.storage)
    }

    pub fn origin(&self) -> &'a D::Text {
        T::get_chunk_origin(self.chunk, self.storage)
    }

    pub fn dropped_trace(&self) -> bool {
        T::get_chunk_dropped_trace(self.chunk, self.storage)
    }

    pub fn trace_id(&self) -> u128 {
        T::get_chunk_trace_id(self.chunk, self.storage)
    }

    pub fn sampling_mechanism(&self) -> u32 {
        T::get_chunk_sampling_mechanism(self.chunk, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::Chunk<'a>>, T::Chunk<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans(&self) -> SpanIterator<'a, T, D, std::slice::Iter<'a, T::Span<'a>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> TraceChunk<'a, T, D, MUT> {
    pub fn set_priority(&self, value: i32) {
        unsafe { T::set_chunk_priority(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_origin<I: Into<D::Text>>(&self, value: I) {
        unsafe { T::set_chunk_origin(as_mut(self.chunk), as_mut(self.storage), value.into()) }
    }

    pub fn set_dropped_trace(&self, value: bool) {
        unsafe { T::set_chunk_dropped_trace(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_trace_id(&self, value: u128) {
        unsafe { T::set_chunk_trace_id(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_sampling_mechanism(&self, value: u32) {
        unsafe { T::set_chunk_sampling_mechanism(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::Chunk<'a>>, T::Chunk<'a>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans_mut(&mut self) -> SpanIteratorMut<'a, T, D, std::slice::Iter<'a, T::Span<'a>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }

    pub fn retain_spans(&mut self, mut predicate: impl FnMut(&mut SpanMut<T, D>) -> bool) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            T::retain_spans(as_mut(self.chunk), as_mut(self.storage), |span, storage|
                predicate(&mut Span {
                    storage,
                    span,
                })
            )
        }
    }

    pub fn add_span(&mut self) -> SpanMut<T, D> {
        Span {
            storage: self.storage,
            span: unsafe { T::add_span(as_mut(self.chunk), as_mut(self.storage)) }
        }
    }
}

pub struct SpanIterator<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Span<'a>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanIteratorMut<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Span<'a>>> = SpanIterator<'a, T, D, I, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::Span<'a>>, const Mut: u8> Iterator for SpanIterator<'a, T, D, I, Mut> {
    type Item = Span<'a, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|span| {
            Span {
                storage: self.storage,
                span,
            }
        })
    }
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
#[derive(Debug)]
pub struct Span<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    span: &'a T::Span<'a>,
}
pub type SpanMut<'a, T, D> = Span<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData> Clone for Span<'_, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Span {
            storage: self.storage,
            span: self.span,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData> Copy for Span<'_, T, D> {}

impl<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8> Span<'a, T, D, Mut> {
    pub fn service(&self) -> &'a D::Text {
        T::get_span_service(self.span, self.storage)
    }

    pub fn name(&self) -> &'a D::Text {
        T::get_span_name(self.span, self.storage)
    }

    pub fn resource(&self) -> &'a D::Text {
        T::get_span_resource(self.span, self.storage)
    }

    pub fn r#type(&self) -> &'a D::Text {
        T::get_span_type(self.span, self.storage)
    }

    pub fn span_id(&self) -> u64 {
        T::get_span_span_id(self.span, self.storage)
    }

    pub fn parent_id(&self) -> u64 {
        T::get_span_parent_id(self.span, self.storage)
    }

    pub fn start(&self) -> i64 {
        T::get_span_start(self.span, self.storage)
    }

    pub fn duration(&self) -> i64 {
        T::get_span_duration(self.span, self.storage)
    }

    pub fn error(&self) -> bool {
        T::get_span_error(self.span, self.storage)
    }

    pub fn env(&self) -> &'a D::Text {
        T::get_span_env(self.span, self.storage)
    }

    pub fn version(&self) -> &'a D::Text {
        T::get_span_version(self.span, self.storage)
    }

    pub fn component(&self) -> &'a D::Text {
        T::get_span_component(self.span, self.storage)
    }

    pub fn kind(&self) -> SpanKind {
        T::get_span_kind(self.span, self.storage)
    }

    pub fn attributes(&self) -> T::AttributeSpan<'_> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links(&self) -> SpanLinkIterator<'a, T, D, std::slice::Iter<'a, T::SpanLink<'a>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn retain_span_links(&mut self, mut predicate: impl FnMut(&mut SpanLinkMut<T, D>) -> bool) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            T::retain_span_links(as_mut(self.span), as_mut(self.storage), |link, storage|
                predicate(&mut SpanLink {
                    storage,
                    link,
                })
            )
        }
    }

    pub fn add_span_link(&mut self) -> SpanLinkMut<T, D> {
        SpanLink {
            storage: self.storage,
            link: unsafe { T::add_span_link(as_mut(self.span), as_mut(self.storage)) }
        }
    }

    pub fn span_events(&self) -> SpanEventIterator<'a, T, D, std::slice::Iter<'a, T::SpanEvent<'a>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }

    pub fn retain_span_events(&mut self, mut predicate: impl FnMut(&mut SpanEventMut<T, D>) -> bool) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            T::retain_span_events(as_mut(self.span), as_mut(self.storage), |event, storage|
                predicate(&mut SpanEvent {
                    storage,
                    event,
                })
            )
        }
    }

    pub fn add_span_event(&mut self) -> SpanEventMut<T, D> {
        SpanEvent {
            storage: self.storage,
            event: unsafe { T::add_span_event(as_mut(self.span), as_mut(self.storage)) }
        }
    }
}

impl <'a, T: TraceProjector<D>, D: TraceData> SpanMut<'a, T, D> {
    pub fn set_service<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_service(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_name<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_name(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_resource<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_resource(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_type<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_type(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_span_id(&mut self, value: u64) {
        unsafe { T::set_span_span_id(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn set_parent_id(&mut self, value: u64) {
        unsafe { T::set_span_parent_id(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn set_start(&mut self, value: i64) {
        unsafe { T::set_span_start(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn set_duration(&mut self, value: i64) {
        unsafe { T::set_span_duration(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn set_error(&mut self, value: bool) {
        unsafe { T::set_span_error(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn set_env<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_env(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_version<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_version(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_component<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_component(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_kind(&mut self, value: SpanKind) {
        unsafe { T::set_span_kind(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::Span<'a>>, T::Span<'a>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links_mut(&mut self) -> SpanLinkIteratorMut<'a, T, D, std::slice::Iter<'a, T::SpanLink<'a>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn span_events_mut(&mut self) -> SpanEventIteratorMut<'a, T, D, std::slice::Iter<'a, T::SpanEvent<'a>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }
}

pub struct SpanLinkIterator<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanLink<'a>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanLinkIteratorMut<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanLink<'a>>> = SpanLinkIterator<'a, T, D, I, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanLink<'a>>, const Mut: u8> Iterator for SpanLinkIterator<'a, T, D, I, Mut> {
    type Item = SpanLink<'a, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|link| {
            SpanLink {
                storage: self.storage,
                link,
            }
        })
    }
}

pub struct SpanEventIterator<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanEvent<'a>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanEventIteratorMut<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanEvent<'a>>> = SpanEventIterator<'a, T, D, I, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceData, I: Iterator<Item = &'a T::SpanEvent<'a>>, const Mut: u8> Iterator for SpanEventIterator<'a, T, D, I, Mut> {
    type Item = SpanEvent<'a, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|event| {
            SpanEvent {
                storage: self.storage,
                event,
            }
        })
    }
}

/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug)]
pub struct SpanLink<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    link: &'a T::SpanLink<'a>,
}
pub type SpanLinkMut<'a, T, D> = SpanLink<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData> Clone for SpanLink<'_, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanLink {
            storage: self.storage,
            link: self.link,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData> Copy for SpanLink<'_, T, D> {}


impl<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8> SpanLink<'a, T, D, Mut> {
    pub fn trace_id(&self) -> u128 {
        T::get_link_trace_id(self.link, self.storage)
    }

    pub fn span_id(&self) -> u64 {
        T::get_link_span_id(self.link, self.storage)
    }

    pub fn trace_state(&self) -> &'a D::Text {
        T::get_link_trace_state(self.link, self.storage)
    }

    pub fn flags(&self) -> u32 {
        T::get_link_flags(self.link, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::SpanLink<'a>>, T::SpanLink<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.link),
            _phantom: PhantomData,
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> SpanLinkMut<'a, T, D> {
    pub fn set_trace_id(&self, value: u128) {
        unsafe { T::set_link_trace_id(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn set_span_id(&self, value: u64) {
        unsafe { T::set_link_span_id(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn set_trace_state<I: Into<D::Text>>(&self, value: I) {
        unsafe { T::set_link_trace_state(as_mut(self.link), as_mut(self.storage), value.into()) }
    }

    pub fn set_flags(&self, value: u32) {
        unsafe { T::set_link_flags(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::SpanLink<'a>>, T::SpanLink<'a>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.link),
            _phantom: PhantomData,
        }
    }
}

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug)]
pub struct SpanEvent<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    event: &'a T::SpanEvent<'a>,
}
pub type SpanEventMut<'a, T, D> = SpanEvent<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData> Clone for SpanEvent<'_, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanEvent {
            storage: self.storage,
            event: self.event,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData> Copy for SpanEvent<'_, T, D> {}

impl<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8> SpanEvent<'a, T, D, Mut> {
    pub fn time_unix_nano(&self) -> u64 {
        T::get_event_time_unix_nano(self.event, self.storage)
    }

    pub fn name(&self) -> &'a D::Text {
        T::get_event_name(self.event, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::SpanEvent<'a>>, T::SpanEvent<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.event),
            _phantom: PhantomData,
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> SpanEventMut<'a, T, D> {
    pub fn set_time_unix_nano(&mut self, value: u64) {
        unsafe { T::set_event_time_unix_nano(as_mut(self.event), as_mut(self.storage), value) }
    }

    pub fn set_name<I: Into<D::Text>>(&mut self, value: I) {
        unsafe { T::set_event_name(as_mut(self.event), as_mut(self.storage), value.into()) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'a, T::SpanEvent<'a>>, T::SpanEvent<'a>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.event),
            _phantom: PhantomData,
        }
    }
}

pub enum AttributeAnyValueType {
    String,
    Bytes,
    Boolean,
    Integer,
    Double,
    Array,
    Map,
}

pub struct AttributeArray<'a, T: TraceProjector<D>, D: TraceData, C: 'a, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: C,
}
pub type AttributeArrayMut<'a, T: TraceProjector<D>, D: TraceData, C: 'a> = AttributeArray<'a, T, D, C, MUT>;

impl<T: TraceProjector<D>, D: TraceData, C: Clone> Clone for AttributeArray<'_, T, D, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        AttributeArray {
            storage: self.storage,
            container: self.container.clone(),
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData, C: Copy> Copy for AttributeArray<'_, T, D, C> {}

pub trait AttributeArrayOp<T: TraceProjector<D>, D: TraceData>: Sized {
    fn get_attribute_array_len(&self, storage: &T::Storage<'_>) -> usize;
    fn get_attribute_array_value<'a>(&'a self, storage: &'a T::Storage<'a>, index: usize) -> AttributeAnyGetterContainer<'a, TraceAttributes<'a, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<T: TraceProjector<D>, D: TraceData> AttributeArrayOp<T, D> for () {
    fn get_attribute_array_len(&self, storage: &T::Storage<'_>) -> usize {
        0
    }

    fn get_attribute_array_value<'a>(&'a self, _storage: &'a T::Storage<'a>, _index: usize) -> AttributeAnyGetterContainer<'a, TraceAttributes<'a, T, D, AttrOwned<()>, ()>, T, D, ()> {
        panic!("AttributeArrayOp::get_attribute_array_value called on empty array")
    }
}

pub trait AttributeArrayMutOp<T: TraceProjector<D>, D: TraceData>: AttributeArrayOp<T, D> {
    fn get_attribute_array_value_mut(&mut self, storage: &mut T::Storage<'_>, index: usize) -> Option<AttributeAnySetterContainer<TraceAttributesMut<T, D, AttrOwned<Self>, Self>, T, D, Self>>;
    fn append_attribute_array_value(&mut self, storage: &mut T::Storage<'_>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<TraceAttributesMut<T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<T: TraceProjector<D>, D: TraceData> AttributeArrayMutOp<T, D> for () {
    fn get_attribute_array_value_mut(&mut self, storage: &mut T::Storage<'_>, index: usize) -> Option<AttributeAnySetterContainer<TraceAttributesMut<T, D, AttrOwned<()>, ()>, T, D, Self>> {
        None
    }

    fn append_attribute_array_value(&mut self, storage: &mut T::Storage<'_>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<TraceAttributesMut<T, D, AttrOwned<()>, ()>, T, D, ()> {
        match value {
            AttributeAnyValueType::String => AttributeAnyContainer::String(()),
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(()),
            AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(()),
            AttributeAnyValueType::Double => AttributeAnyContainer::Double(()),
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, C, const Mut: u8> AttributeArray<'a, T, D, C, Mut>
where
    C: AttributeArrayOp<T, D>,
    TraceAttributes<'a, T, D, AttrOwned<C>, C>: TraceAttributesOp<T, D, C>,
{
    fn len(&self) -> usize {
        self.container.get_attribute_array_len(self.storage)
    }

    fn get(&'a self, index: usize) -> AttributeAnyGetterContainer<'a, TraceAttributes<'a, T, D, AttrOwned<C>, C>, T, D, C> {
        self.container.get_attribute_array_value(self.storage, index)
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, C> AttributeArrayMut<'a, T, D, C>
where
    C: AttributeArrayMutOp<T, D>,
    TraceAttributesMut<'a, T, D, AttrOwned<C>, C>: TraceAttributesMutOp<T, D, C>,
{
    fn get_mut(&'a mut self, index: usize) -> Option<AttributeAnySetterContainer<'a, TraceAttributesMut<'a, T, D, AttrOwned<C>, C>, T, D, C>> {
        unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) }
    }

    fn append(&'a mut self, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'a, TraceAttributesMut<'a, T, D, AttrOwned<C>, C>, T, D, C> {
        unsafe { self.container.append_attribute_array_value(as_mut(self.storage), value) }
    }
    
    // TODO: retain_mut
}

// TODO MUT iter
impl<'a, T: TraceProjector<D>, D: TraceData, C, const Mut: u8> Iterator for AttributeArray<'a, T, D, C, Mut> {
    type Item = AttributeAnyGetterContainer<'a, TraceAttributes<'a, T, D, AttrOwned<C>, C, Mut>, T, D, C>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

pub enum AttributeAnyContainer<String, Bytes, Boolean, Integer, Double, Array, Map> {
    String(String),
    Bytes(Bytes),
    Boolean(Boolean),
    Integer(Integer),
    Double(Double),
    Array(Array),
    Map(Map),
}

pub type AttributeAnyGetterContainer<'a, A: TraceAttributesOp<T, D, C>, T: TraceProjector<D>, D: TraceData, C: 'a> = AttributeAnyContainer<
    &'a D::Text,
    &'a D::Bytes,
    bool,
    i64,
    f64,
    A::Array,
    A::Map,
>;

pub type AttributeAnySetterContainer<'a, A: TraceAttributesMutOp<T, D, C>, T: TraceProjector<D>, D: TraceData, C: 'a> = AttributeAnyContainer<
    A::MutString,
    A::MutBytes,
    A::MutBoolean,
    A::MutInteger,
    A::MutDouble,
    A::MutArray,
    A::MutMap,
>;

pub type AttributeAnyValue<'a, A: TraceAttributesOp<T, D, C>, T: TraceProjector<D>, D: TraceData, C: 'a> = AttributeAnyContainer<
    &'a D::Text,
    &'a D::Bytes,
    bool,
    i64,
    f64,
    AttributeArray<'a, T, D, A::Array>,
    TraceAttributes<'a, T, D, AttrOwned<A::Map>, A::Map>,
>;

trait AttrVal<C> {
    unsafe fn as_mut(&self) -> &mut C;
    fn as_ref(&self) -> &C;
}

#[derive(Copy, Clone)]
pub struct AttrRef<'a, C>(&'a C);
impl<'a, C> AttrVal<C> for AttrRef<'a, C> {
    unsafe fn as_mut(&self) -> &mut C {
        as_mut(self.0)
    }

    fn as_ref(&self) -> &C {
        self.0
    }
}

pub struct AttrOwned<C>(C);
impl<'a, C: 'a> AttrVal<C> for AttrOwned<C> {
    unsafe fn as_mut(&self) -> &mut C {
        as_mut(&self.0)
    }

    fn as_ref(&self) -> &C {
        &self.0
    }
}

impl<C: Clone> Clone for AttrOwned<C> {
    fn clone(&self) -> Self {
        AttrOwned(self.0.clone())
    }
}

impl<C: Copy> Copy for AttrOwned<C> {}

pub struct TraceAttributes<'a, T: TraceProjector<D>, D: TraceData, V: AttrVal<C>, C, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: V,
    _phantom: PhantomData<C>,
}
pub type TraceAttributesMut<'a, T: TraceProjector<D>, D: TraceData, V: AttrVal<C>, C> = TraceAttributes<'a, T, D, V, C, MUT>;

impl<T: TraceProjector<D>, D: TraceData, V: AttrVal<C> + Clone, C> Clone for TraceAttributes<'_, T, D, V, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceAttributes {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<T: TraceProjector<D>, D: TraceData, A: AttrVal<C> + Copy, C> Copy for TraceAttributes<'_, T, D, A, C> {}

pub trait TraceAttributesOp<T: TraceProjector<D>, D: TraceData, C>
{
    type Array;
    type Map;

    fn get<'a>(container: &'a C, storage: &'a T::Storage<'a>, key: D::Text) -> Option<AttributeAnyGetterContainer<'a, Self, T, D, C>>;
}

impl<'b, T: TraceProjector<D>, D: TraceData, const Mut: u8> TraceAttributesOp<T, D, ()> for TraceAttributes<'b, T, D, AttrOwned<()>, (), Mut> {
    type Array = ();
    type Map = ();

    fn get<'a>(_container: &'a (), _storage: &'a T::Storage<'a>, _key: D::Text) -> Option<AttributeAnyGetterContainer<'a, Self, T, D, ()>> {
        None
    }
}

pub trait TraceAttributesMutOp<T: TraceProjector<D>, D: TraceData, C>: TraceAttributesOp<T, D, C>
where
    Self::MutString: TraceAttributesString<T, D>,
    Self::MutBytes: TraceAttributesBytes<T, D>,
    Self::MutBoolean: TraceAttributesBoolean,
    Self::MutInteger: TraceAttributesInteger,
    Self::MutDouble: TraceAttributesDouble,
{
    type MutString;
    type MutBytes;
    type MutBoolean;
    type MutInteger;
    type MutDouble;
    type MutArray;
    type MutMap;

    fn get_mut<'a>(container: &'a mut C, storage: &'a mut T::Storage<'a>, key: D::Text) -> Option<AttributeAnySetterContainer<'a, Self, T, D, C>>;
    fn set<'a>(container: &'a mut C, storage: &'a mut T::Storage<'a>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'a, Self, T, D, C>;
    fn remove(container: &mut C, storage: &mut T::Storage<'_>, key: D::Text);
}

impl<'a, T: TraceProjector<D>, D: TraceData> TraceAttributesMutOp<T, D, ()> for TraceAttributesMut<'a, T, D, AttrOwned<()>, ()> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<'a>(_container: &'a mut (), _storage: &'a mut T::Storage<'a>, _key: D::Text) -> Option<AttributeAnySetterContainer<'a, Self, T, D, ()>> {
        None
    }

    fn set<'a>(_container: &'a mut (), _storage: &'a mut T::Storage<'a>, _key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'a, Self, T, D, ()> {
        match value {
            AttributeAnyValueType::String => AttributeAnyContainer::String(()),
            AttributeAnyValueType::Bytes => AttributeAnyContainer::Bytes(()),
            AttributeAnyValueType::Boolean => AttributeAnyContainer::Boolean(()),
            AttributeAnyValueType::Integer => AttributeAnyContainer::Integer(()),
            AttributeAnyValueType::Double => AttributeAnyContainer::Double(()),
            AttributeAnyValueType::Array => AttributeAnyContainer::Array(()),
            AttributeAnyValueType::Map => AttributeAnyContainer::Map(()),
        }
    }

    fn remove(_container: &mut (), _storage: &mut T::Storage<'_>, _key: D::Text) {
    }
}

pub trait TraceAttributesString<T: TraceProjector<D>, D: TraceData> {
    fn get<'a>(&self, storage: &'a T::Storage<'a>) -> &'a D::Text;
    fn set(self, storage: &mut T::Storage<'_>, value: D::Text);
}

impl<T: TraceProjector<D>, D: TraceData> TraceAttributesString<T, D> for () {
    fn get<'a>(&self, _storage: &'a T::Storage<'a>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn set(self, _storage: &mut T::Storage<'_>, _value: D::Text) {
    }
}

pub trait TraceAttributesBytes<T: TraceProjector<D>, D: TraceData> {
    fn get<'a>(&self, storage: &'a T::Storage<'a>) -> &'a D::Bytes;
    fn set(self, storage: &mut T::Storage<'_>, value: D::Bytes);
}

impl<T: TraceProjector<D>, D: TraceData> TraceAttributesBytes<T, D> for () {
    fn get<'a>(&self, _storage: &'a T::Storage<'a>) -> &'a D::Bytes {
        D::Bytes::default_ref()
    }

    fn set(self, _storage: &mut T::Storage<'_>, _value: D::Bytes) {
    }
}


pub trait TraceAttributesInteger {
    fn get(&self) -> i64;
    fn set(self, value: i64);
}

impl TraceAttributesInteger for () {
    fn get(&self) -> i64 {
        0
    }

    fn set(self, _value: i64) {
    }
}

pub trait TraceAttributesBoolean {
    fn get(&self) -> bool;
    fn set(self, value: bool);
}

impl TraceAttributesBoolean for () {
    fn get(&self) -> bool {
        false
    }

    fn set(self, _value: bool) {
    }
}

pub trait TraceAttributesDouble {
    fn get(&self) -> f64;
    fn set(self, value: f64);
}

impl TraceAttributesDouble for () {
    fn get(&self) -> f64 {
        0.
    }

    fn set(self, _value: f64) {
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, V: AttrVal<C>, C: 'a> TraceAttributes<'a, T, D, V, C>
where
    Self: TraceAttributesOp<T, D, C>,
{
    fn fetch(&self, key: D::Text) -> Option<AttributeAnyGetterContainer<Self, T, D, C>> {
        <Self as TraceAttributesOp<T, D, C>>::get(self.container.as_ref(), self.storage, key)
    }
    
    pub fn get<K: Into<D::Text>>(&self, key: K) -> Option<AttributeAnyValue<Self, T, D, C>> {
        self.fetch(key.into()).map(|v| match v {
            AttributeAnyContainer::String(text) => AttributeAnyValue::<Self, T, D, C>::String(text),
            AttributeAnyContainer::Bytes(bytes) => AttributeAnyValue::<Self, T, D, C>::Bytes(bytes),
            AttributeAnyContainer::Boolean(boolean) => AttributeAnyValue::<Self, T, D, C>::Boolean(boolean),
            AttributeAnyContainer::Integer(integer) => AttributeAnyValue::<Self, T, D, C>::Integer(integer),
            AttributeAnyContainer::Double(double) => AttributeAnyValue::<Self, T, D, C>::Double(double),
            AttributeAnyContainer::Array(array) => AttributeAnyValue::<Self, T, D, C>::Array(AttributeArray {
                storage: self.storage,
                container: array,
            }),
            AttributeAnyContainer::Map(map) => AttributeAnyValue::<Self, T, D, C>::Map(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(map),
                _phantom: PhantomData,
            }),
        })
    }
    
    pub fn get_string<K: Into<D::Text>>(&self, key: K) -> Option<&D::Text> {
        if let Some(AttributeAnyContainer::String(container)) = self.fetch(key.into()) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bytes<K: Into<D::Text>>(&self, key: K) -> Option<&'a D::Bytes> {
        if let Some(AttributeAnyContainer::Bytes(container)) = self.fetch(key.into()) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_bool<K: Into<D::Text>>(&self, key: K) -> Option<bool> {
        if let Some(AttributeAnyContainer::Boolean(container)) = self.fetch(key.into()) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_int<K: Into<D::Text>>(&self, key: K) -> Option<i64> {
        if let Some(AttributeAnyContainer::Integer(container)) = self.fetch(key.into()) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_double<K: Into<D::Text>>(&self, key: K) -> Option<f64> {
        if let Some(AttributeAnyContainer::Double(container)) = self.fetch(key.into()) {
            Some(container)
        } else {
            None
        }
    }

    pub fn get_array<K: Into<D::Text>>(&self, key: K) -> Option<AttributeArray<T, D, <Self as TraceAttributesOp<T, D, C>>::Array>> {
        if let Some(AttributeAnyContainer::Array(container)) = self.fetch(key.into()) {
            Some(AttributeArray {
                storage: self.storage,
                container,
            })
        } else {
            None
        }
    }


    pub fn get_map<K: Into<D::Text>>(&self, key: K) -> Option<TraceAttributes<T, D, AttrOwned<<Self as TraceAttributesOp<T, D, C>>::Map>, <Self as TraceAttributesOp<T, D, C>>::Map>> {
        if let Some(AttributeAnyContainer::Map(container)) = self.fetch(key.into()) {
            Some(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(container),
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, V: AttrVal<C>, C: 'a> TraceAttributes<'a, T, D, V, C>
where
    Self: TraceAttributesMutOp<T, D, C>,
{
    pub fn set_string<K: Into<D::Text>, Val: Into<D::Text>>(&'a mut self, key: K, value: Val) {
        let AttributeAnyContainer::String(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::String) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bytes<K: Into<D::Text>, Val: Into<D::Bytes>>(&'a mut self, key: K, value: Val) {
        let AttributeAnyContainer::Bytes(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Bytes) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bool<K: Into<D::Text>>(&'a mut self, key: K, value: bool) {
        let AttributeAnyContainer::Boolean(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Boolean) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_int<K: Into<D::Text>>(&'a mut self, key: K, value: i64) {
        let AttributeAnyContainer::Integer(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Integer) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_double<K: Into<D::Text>>(&'a mut self, key: K, value: f64) {
        let AttributeAnyContainer::Double(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Double) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_empty_array<K: Into<D::Text>>(&'a mut self, key: K) -> AttributeArrayMut<T, D, <Self as TraceAttributesMutOp<T, D, C>>::MutArray> {
        let AttributeAnyContainer::Array(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Array) }) else { unreachable!() };
        AttributeArray {
            storage: self.storage,
            container,
        }
    }

    pub fn set_empty_map<K: Into<D::Text>>(&'a mut self, key: K) -> TraceAttributesMut<T, D, AttrOwned<<Self as TraceAttributesMutOp<T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<T, D, C>>::MutMap> {
        let AttributeAnyContainer::Map(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Map) }) else { unreachable!() };
        TraceAttributes {
            storage: self.storage,
            container: AttrOwned(container),
            _phantom: PhantomData,
        }
    }

    pub fn get_array_mut<K: Into<D::Text>>(&'a mut self, key: K) -> Option<AttributeArrayMut<T, D, <Self as TraceAttributesMutOp<T, D, C>>::MutArray>> {
        if let Some(AttributeAnyContainer::Array(container)) = unsafe { Self::get_mut(self.container.as_mut(), as_mut(self.storage), key.into()) } {
            Some(AttributeArray {
                storage: self.storage,
                container,
            })
        } else {
            None
        }
    }


    pub fn get_map_mut<K: Into<D::Text>>(&'a mut self, key: K) -> Option<TraceAttributesMut<T, D, AttrOwned<<Self as TraceAttributesMutOp<T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<T, D, C>>::MutMap>> {
        if let Some(AttributeAnyContainer::Map(container)) = unsafe { Self::get_mut(self.container.as_mut(), as_mut(self.storage), key.into()) } {
            Some(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(container),
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }

    pub fn remove<K: Into<D::Text>>(&'a mut self, key: K) {
        unsafe { <Self as TraceAttributesMutOp<T, D, C>>::remove(self.container.as_mut(), as_mut(self.storage), key.into()); }
    }
}
