use std::hash::Hash;
use std::marker::PhantomData;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{IntoData, OwnedTraceData, SpanDataContents, TraceDataLifetime, ImpliedPredicate, TraceData, SpanText, SpanBytes};

pub trait TraceProjector<D: TraceData>: Sized
    + for<'a, 'b> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'b, Self::Trace<'b>>, Self::Trace<'b>>, Impls: TraceAttributesOp<'b, 'a, Self, D, Self::Trace<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'b, Self::Chunk<'b>>, Self::Chunk<'b>>, Impls: TraceAttributesOp<'b, 'a, Self, D, Self::Chunk<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'b, Self::Span<'b>>, Self::Span<'b>>, Impls: TraceAttributesOp<'b, 'a, Self, D, Self::Span<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'b, Self::SpanLink<'b>>, Self::SpanLink<'b>>, Impls: TraceAttributesOp<'b, 'a, Self, D, Self::SpanLink<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'b, Self::SpanEvent<'b>>, Self::SpanEvent<'b>>, Impls: TraceAttributesOp<'b, 'a, Self, D, Self::SpanEvent<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'b, Self::Trace<'b>>, Self::Trace<'b>>, Impls: TraceAttributesMutOp<'b, 'a, Self, D, Self::Trace<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'b, Self::Chunk<'b>>, Self::Chunk<'b>>, Impls: TraceAttributesMutOp<'b, 'a, Self, D, Self::Chunk<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'b, Self::Span<'b>>, Self::Span<'b>>, Impls: TraceAttributesMutOp<'b, 'a, Self, D, Self::Span<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'b, Self::SpanLink<'b>>, Self::SpanLink<'b>>, Impls: TraceAttributesMutOp<'b, 'a, Self, D, Self::SpanLink<'b>>>
    + for<'a, 'b> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'b, Self::SpanEvent<'b>>, Self::SpanEvent<'b>>, Impls: TraceAttributesMutOp<'b, 'a, Self, D, Self::SpanEvent<'b>>>
{
    type Storage<'a>: 'a;
    type Trace<'a>: 'a;
    type Chunk<'a>: 'a;
    type Span<'a>: 'a;
    type SpanLink<'a>: 'a;
    type SpanEvent<'a>: 'a;

    /*
    type AttributeTrace<'a>: TraceAttributesOp<'a, Self, D, Self::Trace<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeChunk<'a>: TraceAttributesOp<'a, Self, D, Self::Chunk<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpan<'a>: TraceAttributesOp<'a, Self, D, Self::Span<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpanLink<'a>: TraceAttributesOp<'a, Self, D, Self::SpanLink<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpanEvent<'a>: TraceAttributesOp<'a, Self, D, Self::SpanEvent<'a>> + 'a where D: TraceDataLifetime<'a>;
*/
    fn project<'a>(&'a self) -> Traces<Self, D> where D: TraceDataLifetime<'a>;
    fn project_mut<'a>(&'a mut self) -> TracesMut<Self, D> where D: TraceDataLifetime<'a>;

    fn add_chunk<'a>(trace: &'a mut Self::Trace<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::Chunk<'a>;
    fn chunk_iterator<'a>(trace: &'a Self::Trace<'a>) -> std::slice::Iter<'a, Self::Chunk<'a>>;
    fn retain_chunks<'b, 'a, F: for<'c> FnMut(&'c mut Self::Chunk<'c>, &'c mut Self::Storage<'a>) -> bool>(trace: &'b mut Self::Trace<'b>, storage: &'a mut Self::Storage<'a>, predicate: F);
    fn add_span<'a>(chunk: &'a mut Self::Chunk<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::Span<'a>;
    fn span_iterator<'a>(chunk: &'a Self::Chunk<'a>) -> std::slice::Iter<'a, Self::Span<'a>>;
    fn retain_spans<'r, F: FnMut(&mut Self::Span<'r>, &mut Self::Storage<'r>) -> bool>(chunk: &'r mut Self::Chunk<'r>, storage: &'r mut Self::Storage<'r>, predicate: F);
    fn add_span_link<'a>(span: &'a mut Self::Span<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::SpanLink<'a>;
    fn span_link_iterator<'a>(span: &'a Self::Span<'a>) -> std::slice::Iter<'a, Self::SpanLink<'a>>;
    fn retain_span_links<'r, F: FnMut(&mut Self::SpanLink<'r>, &mut Self::Storage<'r>) -> bool>(span: &'r mut Self::Span<'r>, storage: &'r mut Self::Storage<'r>, predicate: F);
    fn add_span_event<'a>(span: &mut Self::Span<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::SpanEvent<'a>;
    fn span_event_iterator<'a>(span: &'a Self::Span<'a>) -> std::slice::Iter<'a, Self::SpanEvent<'a>>;
    fn retain_span_events<'r, F: FnMut(&mut Self::SpanEvent<'r>, &mut Self::Storage<'r>) -> bool>(span: &'r mut Self::Span<'r>, storage: &'r mut Self::Storage<'r>, predicate: F);

    fn get_trace_container_id<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_language_name<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_language_version<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_tracer_version<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_runtime_id<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_env<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_hostname<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_trace_app_version<'a>(trace: &Self::Trace<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;

    fn set_trace_container_id(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_language_name(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_language_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_tracer_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_runtime_id(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_env(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_hostname(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_trace_app_version(trace: &mut Self::Trace<'_>, storage: &mut Self::Storage<'_>, value: D::Text);

    fn get_chunk_priority(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> i32;
    fn get_chunk_origin<'a>(chunk: &Self::Chunk<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_chunk_dropped_trace(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> bool;
    fn get_chunk_trace_id(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> u128;
    fn get_chunk_sampling_mechanism(chunk: &Self::Chunk<'_>, storage: &Self::Storage<'_>) -> u32;

    fn set_chunk_priority(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: i32);
    fn set_chunk_origin(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_chunk_dropped_trace(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: bool);
    fn set_chunk_trace_id(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: u128) where D: OwnedTraceData;
    fn set_chunk_sampling_mechanism(chunk: &mut Self::Chunk<'_>, storage: &mut Self::Storage<'_>, value: u32);

    fn get_span_service<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_name<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_resource<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_type<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_span_id(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_span_parent_id(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_span_start(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> i64;
    fn get_span_duration(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> i64;
    fn get_span_error(span: &Self::Span<'_>, storage: &Self::Storage<'_>) -> bool;
    fn get_span_env<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_version<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_span_component<'a>(span: &Self::Span<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
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
    fn get_link_trace_state<'a>(link: &Self::SpanLink<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;
    fn get_link_flags(link: &Self::SpanLink<'_>, storage: &Self::Storage<'_>) -> u32;

    fn set_link_trace_id(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u128);
    fn set_link_span_id(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_link_trace_state(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
    fn set_link_flags(link: &mut Self::SpanLink<'_>, storage: &mut Self::Storage<'_>, value: u32);

    fn get_event_time_unix_nano(event: &Self::SpanEvent<'_>, storage: &Self::Storage<'_>) -> u64;
    fn get_event_name<'a>(event: &Self::SpanEvent<'_>, storage: &'a Self::Storage<'a>) -> &'a D::Text;

    fn set_event_time_unix_nano(event: &mut Self::SpanEvent<'_>, storage: &mut Self::Storage<'_>, value: u64);
    fn set_event_name(event: &mut Self::SpanEvent<'_>, storage: &mut Self::Storage<'_>, value: D::Text);
}

pub const IMMUT: u8 = 0;
pub const MUT: u8 = 1;

#[allow(invalid_reference_casting)]
unsafe fn as_mut<T>(v: &T) -> &mut T {
    &mut *(v as *const _ as *mut _)
}

struct TraceValue<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: &'a C,
}

#[derive(Debug)]
pub struct Traces<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    traces: &'a T::Trace<'a>,
}
pub type TracesMut<'a, T, D> = Traces<'a, T, D, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for Traces<'a, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Traces {
            storage: self.storage,
            traces: self.traces,
        }
    }
}
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for Traces<'a, T, D> {}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Traces<'a, T, D> {
    pub fn new(traces: &'a T::Trace<'a>, storage: &'a T::Storage<'a>) -> Self {
        Self::generic_new(traces, storage)
    }
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> Traces<'a, T, D, Mut> {
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

    pub fn chunks(&self) -> ChunkIterator<'a, 'a, 'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TracesMut<'a, T, D> {
    pub fn new_mut(traces: &'a mut T::Trace<'a>, storage: &'a mut T::Storage<'a>) -> Self {
        Self::generic_new(traces, storage)
    }

    pub fn set_container_id<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_container_id(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_language_name<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_language_name(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_language_version<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_language_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_tracer_version<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_tracer_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_runtime_id<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_runtime_id(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_env<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_env(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_hostname<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_hostname(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn set_app_version<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_trace_app_version(as_mut(self.traces), as_mut(self.storage), value.into()) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'a, T, D, AttrRef<'a, T::Trace<'a>>, T::Trace<'a>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.traces),
            _phantom: PhantomData,
        }
    }

    pub fn chunks_mut(&mut self) -> ChunkIteratorMut<'a, 'a, 'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }

    pub fn retain_chunks<F: FnMut(&mut TraceChunkMut<'a, 'a, 'a, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let traces = as_mut(self.traces);
            let storage_ref: &'a mut T::Storage<'a> = as_mut(self.storage);
            T::retain_chunks(traces, storage_ref, move |chunk, storage| {
                let mut trace_chunk: TraceChunkMut<'_, '_, 'a, T, D> = TraceChunkMut { storage, chunk };
                let chunk_ref: &mut TraceChunkMut<'a, 'a, 'a, T, D> = std::mem::transmute(&mut trace_chunk);
                predicate(chunk_ref)
            })
        }
    }

    pub fn add_chunk(&mut self) -> TraceChunkMut<'a, 'a, 'a, T, D> {
        TraceChunk {
            storage: self.storage,
            chunk: unsafe { T::add_chunk(as_mut(self.traces), as_mut(self.storage)) },
        }
    }
}

pub struct ChunkIterator<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    it: I,
}
pub type ChunkIteratorMut<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>> = ChunkIterator<'b, 's, 'a, T, D, I, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>, const Mut: u8> Iterator for ChunkIterator<'b, 's, 'a, T, D, I, Mut> {
    type Item = TraceChunk<'b, 's, 'a, T, D, Mut>;

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
pub struct TraceChunk<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    chunk: &'b T::Chunk<'b>,
}
pub type TraceChunkMut<'b, 's, 'a, T, D> = TraceChunk<'b, 's, 'a, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for TraceChunk<'b, 's, 'a, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceChunk {
            storage: self.storage,
            chunk: self.chunk,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for TraceChunk<'b, 's, 'a, T, D> {}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> TraceChunk<'b, 's, 'a, T, D, Mut> where 's: 'a {
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

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::Chunk<'b>>, T::Chunk<'b>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans(&self) -> SpanIterator<'b, 's, 'a, T, D, std::slice::Iter<'b, T::Span<'b>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }
}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceChunk<'b, 's, 'a, T, D, MUT> where 's: 'a {
    pub fn set_priority(&self, value: i32) {
        unsafe { T::set_chunk_priority(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_origin<I: IntoData<D::Text>>(&self, value: I) {
        unsafe { T::set_chunk_origin(as_mut(self.chunk), as_mut(self.storage), value.into()) }
    }

    pub fn set_dropped_trace(&self, value: bool) {
        unsafe { T::set_chunk_dropped_trace(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_trace_id(&self, value: u128) where D: OwnedTraceData {
        unsafe { T::set_chunk_trace_id(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn set_sampling_mechanism(&self, value: u32) {
        unsafe { T::set_chunk_sampling_mechanism(as_mut(self.chunk), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::Chunk<'b>>, T::Chunk<'b>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans_mut(&mut self) -> SpanIteratorMut<'b, 's, 'a, T, D, std::slice::Iter<'b, T::Span<'b>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn retain_spans<F: FnMut(&mut SpanMut<'a, 'a, 'a, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let chunk: &'a mut T::Chunk<'a> = std::mem::transmute(self.chunk);
            let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(self.storage);
            T::retain_spans(chunk, storage_ref, |span, storage| {
                let span_ref: &'a mut T::Span<'a> = std::mem::transmute(span);
                let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(storage);
                let mut span_obj = Span::<'a, 'a, 'a, T, D, MUT> { storage: storage_ref, span: span_ref };
                predicate(&mut span_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span(&mut self) -> Span<'b, 's, 'a, T, D, MUT> {
        unsafe {
            let chunk: &'a mut T::Chunk<'a> = std::mem::transmute(self.chunk);
            let storage_transmuted: &mut T::Storage<'a> = std::mem::transmute(self.storage);
            let span_ref = T::add_span(chunk, storage_transmuted);
            Span {
                storage: self.storage,
                span: std::mem::transmute(span_ref)
            }
        }
    }
}

pub struct SpanIterator<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    it: I,
}
pub type SpanIteratorMut<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>> = SpanIterator<'b, 's, 'a, T, D, I, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>, const Mut: u8> Iterator for SpanIterator<'b, 's, 'a, T, D, I, Mut> {
    type Item = Span<'b, 's, 'a, T, D, Mut>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |span| {
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
pub struct Span<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    span: &'b T::Span<'b>,
}
pub type SpanMut<'b, 's, 'a, T, D> = Span<'b, 's, 'a, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for Span<'b, 's, 'a, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Span {
            storage: self.storage,
            span: self.span,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for Span<'b, 's, 'a, T, D> {}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> Span<'b, 's, 'a, T, D, Mut> where 's: 'a {
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

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::Span<'b>>, T::Span<'b>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links(&self) -> SpanLinkIterator<'b, 's, 'a, T, D, std::slice::Iter<'b, T::SpanLink<'b>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    #[allow(mutable_transmutes)]
    pub fn retain_span_links<F: FnMut(&mut SpanLinkMut<'a, 'a, 'a, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'a mut T::Span<'a> = std::mem::transmute(self.span);
            let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(self.storage);
            T::retain_span_links(span, storage_ref, |link, storage| {
                let link_ref: &'a mut T::SpanLink<'a> = std::mem::transmute(link);
                let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(storage);
                let mut link_obj = SpanLink::<'a, 'a, 'a, T, D, MUT> { storage: storage_ref, link: link_ref };
                predicate(&mut link_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span_link(&mut self) -> SpanLink<'b, 's, 'a, T, D, MUT> {
        unsafe {
            let span: &'a mut T::Span<'a> = std::mem::transmute(self.span);
            let storage_transmuted: &mut T::Storage<'a> = std::mem::transmute(self.storage);
            let link_ref = T::add_span_link(span, storage_transmuted);
            SpanLink {
                storage: self.storage,
                link: std::mem::transmute(link_ref)
            }
        }
    }

    pub fn span_events(&self) -> SpanEventIterator<'b, 's, 'a, T, D, std::slice::Iter<'b, T::SpanEvent<'b>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }

    #[allow(mutable_transmutes)]
    pub fn retain_span_events<F: FnMut(&mut SpanEventMut<'a, 'a, 'a, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'a mut T::Span<'a> = std::mem::transmute(self.span);
            let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(self.storage);
            T::retain_span_events(span, storage_ref, |event, storage| {
                let event_ref: &'a mut T::SpanEvent<'a> = std::mem::transmute(event);
                let storage_ref: &'a mut T::Storage<'a> = std::mem::transmute(storage);
                let mut event_obj = SpanEvent::<'a, 'a, 'a, T, D, MUT> { storage: storage_ref, event: event_ref };
                predicate(&mut event_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span_event(&mut self) -> SpanEvent<'b, 's, 'a, T, D, MUT> {
        unsafe {
            let span: &mut T::Span<'a> = std::mem::transmute(self.span);
            let storage_transmuted: &mut T::Storage<'a> = std::mem::transmute(self.storage);
            let event_ref = T::add_span_event(span, storage_transmuted);
            SpanEvent {
                storage: self.storage,
                event: std::mem::transmute(event_ref)
            }
        }
    }
}

impl <'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanMut<'b, 's, 'a, T, D> where 's: 'a {
    pub fn set_service<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_service(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_name<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_name(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_resource<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_resource(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_type<I: IntoData<D::Text>>(&mut self, value: I) {
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

    pub fn set_env<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_env(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_version<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_version(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_component<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_span_component(as_mut(self.span), as_mut(self.storage), value.into()) }
    }

    pub fn set_kind(&mut self, value: SpanKind) {
        unsafe { T::set_span_kind(as_mut(self.span), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::Span<'b>>, T::Span<'b>, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links_mut(&mut self) -> SpanLinkIteratorMut<'b, 's, 'a, T, D, std::slice::Iter<'b, T::SpanLink<'b>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn span_events_mut(&mut self) -> SpanEventIteratorMut<'b, 's, 'a, T, D, std::slice::Iter<'b, T::SpanEvent<'b>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }
}

pub struct SpanLinkIterator<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    it: I,
}
pub type SpanLinkIteratorMut<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>> = SpanLinkIterator<'b, 's, 'a, T, D, I, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>, const Mut: u8> Iterator for SpanLinkIterator<'b, 's, 'a, T, D, I, Mut> {
    type Item = SpanLink<'b, 's, 'a, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |link| {
            SpanLink {
                storage: self.storage,
                link,
            }
        })
    }
}

pub struct SpanEventIterator<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    it: I,
}
pub type SpanEventIteratorMut<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>> = SpanEventIterator<'b, 's, 'a, T, D, I, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>, const Mut: u8> Iterator for SpanEventIterator<'b, 's, 'a, T, D, I, Mut> {
    type Item = SpanEvent<'b, 's, 'a, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |event| {
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
pub struct SpanLink<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    link: &'b T::SpanLink<'b>,
}
pub type SpanLinkMut<'b, 's, 'a, T, D> = SpanLink<'b, 's, 'a, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for SpanLink<'b, 's, 'a, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanLink {
            storage: self.storage,
            link: self.link,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for SpanLink<'b, 's, 'a, T, D> {}


impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> SpanLink<'b, 's, 'a, T, D, Mut> where 's: 'a {
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

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::SpanLink<'b>>, T::SpanLink<'b>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.link),
            _phantom: PhantomData,
        }
    }
}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanLinkMut<'b, 's, 'a, T, D> where 's: 'a {
    pub fn set_trace_id(&self, value: u128) {
        unsafe { T::set_link_trace_id(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn set_span_id(&self, value: u64) {
        unsafe { T::set_link_span_id(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn set_trace_state<I: IntoData<D::Text>>(&self, value: I) {
        unsafe { T::set_link_trace_state(as_mut(self.link), as_mut(self.storage), value.into()) }
    }

    pub fn set_flags(&self, value: u32) {
        unsafe { T::set_link_flags(as_mut(self.link), as_mut(self.storage), value) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::SpanLink<'b>>, T::SpanLink<'b>, MUT> {
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
pub struct SpanEvent<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'s T::Storage<'a>,
    event: &'b T::SpanEvent<'b>,
}
pub type SpanEventMut<'b, 's, 'a, T, D> = SpanEvent<'b, 's, 'a, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for SpanEvent<'b, 's, 'a, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanEvent {
            storage: self.storage,
            event: self.event,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for SpanEvent<'b, 's, 'a, T, D> {}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> SpanEvent<'b, 's, 'a, T, D, Mut> where 's: 'a {
    pub fn time_unix_nano(&self) -> u64 {
        T::get_event_time_unix_nano(self.event, self.storage)
    }

    pub fn name(&self) -> &'a D::Text {
        T::get_event_name(self.event, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::SpanEvent<'b>>, T::SpanEvent<'b>> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.event),
            _phantom: PhantomData,
        }
    }
}

impl<'b, 's, 'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanEventMut<'b, 's, 'a, T, D> where 's: 'a {
    pub fn set_time_unix_nano(&mut self, value: u64) {
        unsafe { T::set_event_time_unix_nano(as_mut(self.event), as_mut(self.storage), value) }
    }

    pub fn set_name<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_event_name(as_mut(self.event), as_mut(self.storage), value.into()) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'a, T, D, AttrRef<'b, T::SpanEvent<'b>>, T::SpanEvent<'b>, MUT> {
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

pub struct AttributeArray<'c, 'a: 'c, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: 'c, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: C,
    _phantom: PhantomData<&'c C>,
}
pub type AttributeArrayMut<'c, 'a: 'c, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: 'c> = AttributeArray<'c, 'a, T, D, C, MUT>;

impl<'a: 'c, 'c, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: Clone> Clone for AttributeArray<'c, 'a, T, D, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        AttributeArray {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'a: 'c, 'c, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: Copy> Copy for AttributeArray<'c, 'a, T, D, C> {}

pub trait AttributeArrayOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: Sized + ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesOp<'container, 'storage, T, D, Self>> + 'container
{
    fn get_attribute_array_len(&self, storage: &T::Storage<'storage>) -> usize;
    fn get_attribute_array_value(&'container self, storage: &T::Storage<'storage>, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> AttributeArrayOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_len(&self, _storage: &T::Storage<'storage>) -> usize {
        0
    }

    fn get_attribute_array_value(&self, _storage: &T::Storage<'storage>, _index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
        panic!("AttributeArrayOp::get_attribute_array_value called on empty array")
    }
}

pub trait AttributeArrayMutOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D> + ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self>> + 'container
{
    fn get_attribute_array_value_mut(&'container mut self, storage: &mut T::Storage<'storage>, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>>;
    fn append_attribute_array_value(&'container mut self, storage: &mut T::Storage<'storage>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> AttributeArrayMutOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_value_mut(&'container mut self, _storage: &mut T::Storage<'storage>, _index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, Self>> {
        None
    }

    fn append_attribute_array_value(&'container mut self, _storage: &mut T::Storage<'storage>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
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

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container, const Mut: u8> AttributeArray<'container, 'storage, T, D, C, Mut>
where
    C: AttributeArrayOp<'container, 'storage, T, D>,
{
    fn len(&self) -> usize {
        self.container.get_attribute_array_len(self.storage)
    }

    fn get(&'container self, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<C>, C>, T, D, C> {
        self.container.get_attribute_array_value(self.storage, index)
    }
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> AttributeArrayMut<'container, 'storage, T, D, C>
where
    C: AttributeArrayMutOp<'container, 'storage, T, D>,
{
    fn get_mut(&'container mut self, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C>> {
        unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) }
    }

    fn append(&'container mut self, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C> {
        unsafe { self.container.append_attribute_array_value(as_mut(self.storage), value) }
    }

    // TODO: retain_mut
}

// TODO MUT iter
impl<'storage, 'container, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container, const Mut: u8> Iterator for AttributeArray<'container, 'storage, T, D, C, Mut>
where
    TraceAttributes<'storage, T, D, AttrOwned<C>, C, Mut>: TraceAttributesOp<'container, 'storage, T, D, C>,
{
    type Item = AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<C>, C, Mut>, T, D, C>;

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

pub type AttributeAnyGetterContainer<'container, 'storage, A: TraceAttributesOp<'container, 'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    A::Array,
    A::Map,
>;

pub type AttributeAnySetterContainer<'container, 'storage, A: TraceAttributesMutOp<'container, 'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    A::MutString,
    A::MutBytes,
    A::MutBoolean,
    A::MutInteger,
    A::MutDouble,
    A::MutArray,
    A::MutMap,
>;

pub type AttributeAnyValue<'container, 'storage, A: TraceAttributesOp<'container, 'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    AttributeArray<'container, 'storage, T, D, A::Array>,
    TraceAttributes<'storage, T, D, AttrOwned<A::Map>, A::Map>,
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

pub struct TraceAttributes<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, V: AttrVal<C>, C, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: V,
    _phantom: PhantomData<C>,
}
pub type TraceAttributesMut<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, V: AttrVal<C>, C> = TraceAttributes<'a, T, D, V, C, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, V: AttrVal<C> + Clone, C> Clone for TraceAttributes<'a, T, D, V, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceAttributes {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, A: AttrVal<C> + Copy, C> Copy for TraceAttributes<'a, T, D, A, C> {}

// Helper traits to break the recursion cycle in TraceAttributesOp
pub trait ArrayAttributesOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D>
{}

pub trait MapAttributesOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesOp<'container, 'storage, T, D> for C
where
    TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

pub trait TraceAttributesOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container>
{
    type Array: ArrayAttributesOp<'container, 'storage, T, D>;
    type Map;

    fn get<K>(container: &'container C, storage: &'storage T::Storage<'storage>, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;

    fn get_double<K>(container: &'container C, storage: &'storage T::Storage<'storage>, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        Self::get(container, storage, key).and_then(|v| match v {
            AttributeAnyContainer::Double(d) => Some(d),
            _ => None,
        })
    }
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, const Mut: u8> TraceAttributesOp<'container, 'storage, T, D, ()> for TraceAttributes<'storage, T, D, AttrOwned<()>, (), Mut> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'container (), _storage: &'storage T::Storage<'storage>, _key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

// Helper traits to break the recursion cycle in TraceAttributesMutOp
pub trait ArrayAttributesMutOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: AttributeArrayMutOp<'container, 'storage, T, D>
{}

pub trait MapAttributesMutOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesMutOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayMutOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesMutOp<'container, 'storage, T, D> for C
where
    TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesMutOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

pub trait TraceAttributesMutOp<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container>: TraceAttributesOp<'container, 'storage, T, D, C>
where
    Self::MutString: TraceAttributesString<'storage, T, D>,
    Self::MutBytes: TraceAttributesBytes<'storage, T, D>,
    Self::MutBoolean: TraceAttributesBoolean,
    Self::MutInteger: TraceAttributesInteger,
    Self::MutDouble: TraceAttributesDouble,
{
    type MutString;
    type MutBytes;
    type MutBoolean;
    type MutInteger;
    type MutDouble;
    type MutArray: ArrayAttributesMutOp<'container, 'storage, T, D>;
    type MutMap;

    fn get_mut<K>(container: &'container mut C, storage: &mut T::Storage<'storage>, key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
    fn set(container: &'container mut C, storage: &mut T::Storage<'storage>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>;
    fn remove<K>(container: &mut C, storage: &mut T::Storage<'storage>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> TraceAttributesMutOp<'container, 'storage, T, D, ()> for TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'container mut (), _storage: &mut T::Storage<'storage>, _key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }

    fn set(_container: &'container mut (), _storage: &mut T::Storage<'storage>, _key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()> {
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

    fn remove<K>(_container: &mut (), _storage: &mut T::Storage<'storage>, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

pub trait TraceAttributesString<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> {
    fn get(&self, storage: &'a T::Storage<'a>) -> &'a D::Text;
    fn set(self, storage: &mut T::Storage<'a>, value: D::Text);
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceAttributesString<'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage<'a>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn set(self, _storage: &mut T::Storage<'a>, _value: D::Text) {
    }
}

pub trait TraceAttributesBytes<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> {
    fn get(&self, storage: &'a T::Storage<'a>) -> &'a D::Bytes;
    fn set(self, storage: &mut T::Storage<'a>, value: D::Bytes);
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceAttributesBytes<'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage<'a>) -> &'a D::Bytes {
        D::Bytes::default_ref()
    }

    fn set(self, _storage: &mut T::Storage<'a>, _value: D::Bytes) {
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

// Simplified methods that work without the complex TraceAttributesOp bound
impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>: TraceAttributesOp<'container, 'storage, T, D, C>,
{
    pub fn get_double<K>(self, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::get_double(self.container.0, self.storage, key)
    }
}

// Simplified mutable methods
impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>: TraceAttributesMutOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_double<K: IntoData<D::Text>>(&mut self, key: K, value: f64) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Double(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Double) else { unreachable!() };
        container.set(value)
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn remove<K>(&mut self, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::remove(container_ref, storage_ref, key);
    }
}

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributes<'storage, T, D, V, C>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>: TraceAttributesOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting)]
    fn fetch<K>(&self, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let container_ref: &'container C = unsafe { &*(self.container.as_ref() as *const _ as *const C) };
        <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::get(container_ref, self.storage, key)
    }

    pub fn get<K>(&self, key: &K) -> Option<AttributeAnyValue<'container, 'storage, TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        self.fetch(key).map(move |v| match v {
            AttributeAnyContainer::String(text) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::String(text),
            AttributeAnyContainer::Bytes(bytes) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Bytes(bytes),
            AttributeAnyContainer::Boolean(boolean) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Boolean(boolean),
            AttributeAnyContainer::Integer(integer) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Integer(integer),
            AttributeAnyContainer::Double(double) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Double(double),
            AttributeAnyContainer::Array(array) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Array(AttributeArray {
                storage: self.storage,
                container: array,
                _phantom: PhantomData,
            }),
            AttributeAnyContainer::Map(map) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Map(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(map),
                _phantom: PhantomData,
            }),
        })
    }

    #[allow(invalid_reference_casting)]
    pub fn get_string<K>(&self, key: &K) -> Option<&'storage D::Text>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::String(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    #[allow(invalid_reference_casting)]
    pub fn get_bytes<K>(&self, key: &K) -> Option<&'storage D::Bytes>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Bytes(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    #[allow(invalid_reference_casting)]
    pub fn get_bool<K>(&self, key: &K) -> Option<bool>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Boolean(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    #[allow(invalid_reference_casting)]
    pub fn get_int<K>(&self, key: &K) -> Option<i64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Integer(container)) = self.fetch(key) {
            Some(container)
        } else {
            None
        }
    }

    #[allow(invalid_reference_casting)]
    pub fn get_array<K>(&self, key: &K) -> Option<AttributeArray<'container, 'storage, T, D, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::Array>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Array(container)) = self.fetch(key) {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }


    #[allow(invalid_reference_casting)]
    pub fn get_map<K>(&self, key: &K) -> Option<TraceAttributes<'storage, T, D, AttrOwned<<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::Map>, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'container, 'storage, T, D, C>>::Map>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Map(container)) = self.fetch(key) {
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

impl<'container, 'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributesMut<'storage, T, D, V, C>
where
    D::Text: Clone + From<String> + for<'b> From<&'b str>,
    D::Bytes: Clone + From<Vec<u8>> + for<'b> From<&'b [u8]>,
    Self: TraceAttributesMutOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_string<K: IntoData<D::Text>, Val: IntoData<D::Text>>(&mut self, key: K, value: Val) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::String(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::String) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_bytes<K: IntoData<D::Text>, Val: IntoData<D::Bytes>>(&mut self, key: K, value: Val) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Bytes(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Bytes) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_bool<K: IntoData<D::Text>>(&mut self, key: K, value: bool) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Boolean(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Boolean) else { unreachable!() };
        container.set(value)
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_empty_array<K: IntoData<D::Text>>(&mut self, key: K) -> AttributeArrayMut<'container, 'storage, T, D, <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutArray> {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Array(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Array) else { unreachable!() };
        AttributeArray {
            storage: self.storage,
            container,
            _phantom: PhantomData,
        }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_empty_map<K: IntoData<D::Text>>(&mut self, key: K) -> TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutMap> {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Map(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Map) else { unreachable!() };
        TraceAttributes {
            storage: self.storage,
            container: AttrOwned(container),
            _phantom: PhantomData,
        }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn get_array_mut<K>(&mut self, key: &K) -> Option<AttributeArrayMut<'container, 'storage, T, D, <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutArray>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        if let Some(AttributeAnyContainer::Array(container)) = Self::get_mut(container_ref, storage_ref, key) {
            Some(AttributeArray {
                storage: self.storage,
                container,
                _phantom: PhantomData,
            })
        } else {
            None
        }
    }


    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn get_map_mut<K>(&mut self, key: &K) -> Option<TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutMap>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage<'storage> = unsafe { as_mut(self.storage) };
        if let Some(AttributeAnyContainer::Map(container)) = Self::get_mut(container_ref, storage_ref, key) {
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
