use std::hash::Hash;
use std::marker::PhantomData;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{IntoData, OwnedTraceData, SpanDataContents, TraceDataLifetime, ImpliedPredicate, TraceData, SpanText, SpanBytes};

pub trait TraceProjector<D: TraceData>: Sized
    + for<'a> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'a, Self::Trace<'a>>, Self::Trace<'a>>, Impls: TraceAttributesOp<'a, Self, D, Self::Trace<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'a, Self::Chunk<'a>>, Self::Chunk<'a>>, Impls: TraceAttributesOp<'a, Self, D, Self::Chunk<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'a, Self::Span<'a>>, Self::Span<'a>>, Impls: TraceAttributesOp<'a, Self, D, Self::Span<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'a, Self::SpanLink<'a>>, Self::SpanLink<'a>>, Impls: TraceAttributesOp<'a, Self, D, Self::SpanLink<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributes<'a, Self, D, AttrRef<'a, Self::SpanEvent<'a>>, Self::SpanEvent<'a>>, Impls: TraceAttributesOp<'a, Self, D, Self::SpanEvent<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'a, Self::Trace<'a>>, Self::Trace<'a>>, Impls: TraceAttributesMutOp<'a, Self, D, Self::Trace<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'a, Self::Chunk<'a>>, Self::Chunk<'a>>, Impls: TraceAttributesMutOp<'a, Self, D, Self::Chunk<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'a, Self::Span<'a>>, Self::Span<'a>>, Impls: TraceAttributesMutOp<'a, Self, D, Self::Span<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'a, Self::SpanLink<'a>>, Self::SpanLink<'a>>, Impls: TraceAttributesMutOp<'a, Self, D, Self::SpanLink<'a>>>
    + for<'a> ImpliedPredicate<TraceAttributesMut<'a, Self, D, AttrRef<'a, Self::SpanEvent<'a>>, Self::SpanEvent<'a>>, Impls: TraceAttributesMutOp<'a, Self, D, Self::SpanEvent<'a>>>
{
    type Storage<'a>: 'a;
    type Trace<'a>: 'a;
    type Chunk<'a>: 'a;
    type Span<'a>: 'a;
    type SpanLink<'a>: 'a;
    type SpanEvent<'a>: 'a;

    type AttributeTrace<'a>: TraceAttributesOp<'a, Self, D, Self::Trace<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeChunk<'a>: TraceAttributesOp<'a, Self, D, Self::Chunk<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpan<'a>: TraceAttributesOp<'a, Self, D, Self::Span<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpanLink<'a>: TraceAttributesOp<'a, Self, D, Self::SpanLink<'a>> + 'a where D: TraceDataLifetime<'a>;
    type AttributeSpanEvent<'a>: TraceAttributesOp<'a, Self, D, Self::SpanEvent<'a>> + 'a where D: TraceDataLifetime<'a>;

    fn project<'a>(&'a self) -> Traces<Self, D> where D: TraceDataLifetime<'a>;
    fn project_mut<'a>(&'a mut self) -> TracesMut<Self, D> where D: TraceDataLifetime<'a>;

    fn add_chunk<'a>(trace: &'a mut Self::Trace<'a>, storage: &mut Self::Storage<'a>) -> &'a mut Self::Chunk<'a>;
    fn chunk_iterator<'a>(trace: &'a Self::Trace<'a>) -> std::slice::Iter<'a, Self::Chunk<'a>>;
    fn retain_chunks<'r, 'a, F: FnMut(&'a mut Self::Chunk<'a>, &'a mut Self::Storage<'a>) -> bool>(trace: &'r mut Self::Trace<'r>, storage: &'r mut Self::Storage<'r>, predicate: F) where 'a: 'r;
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

    pub fn chunks(&self) -> ChunkIterator<'a, 'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
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

    pub fn chunks_mut(&mut self) -> ChunkIteratorMut<'a, 'a, T, D, std::slice::Iter<'a, T::Chunk<'a>>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }

    pub fn retain_chunks<'b, F: FnMut(&mut TraceChunkMut<'a, 'b, T, D>) -> bool>(&mut self, mut predicate: F) where 'a: 'b {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let traces: &'a mut T::Trace<'a> = as_mut(self.traces);
            let storage_ref: &'a mut T::Storage<'a> = as_mut(self.storage);
            T::retain_chunks(traces, storage_ref, move |chunk, storage| {
                let mut trace_chunk = TraceChunkMut::<'a, '_, T, D> { storage, chunk };
                //let chunk_ref: &mut TraceChunkMut<'a, T, D> = std::mem::transmute(&mut trace_chunk);
                //predicate(chunk_ref)
                predicate(&mut trace_chunk)
            })
        }
    }

    pub fn add_chunk(&mut self) -> TraceChunkMut<'a, 'a, T, D> {
        TraceChunk {
            storage: self.storage,
            chunk: unsafe { T::add_chunk(as_mut(self.traces), as_mut(self.storage)) },
        }
    }
}

pub struct ChunkIterator<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type ChunkIteratorMut<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>> = ChunkIterator<'a, 'b, T, D, I, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Chunk<'b>>, const Mut: u8> Iterator for ChunkIterator<'a, 'b, T, D, I, Mut> {
    type Item = TraceChunk<'a, 'b, T, D, Mut>;

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
pub struct TraceChunk<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    chunk: &'b T::Chunk<'b>,
}
pub type TraceChunkMut<'a: 'b, 'b, T, D> = TraceChunk<'a, 'b, T, D, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for TraceChunk<'a, 'b, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceChunk {
            storage: self.storage,
            chunk: self.chunk,
        }
    }
}
impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for TraceChunk<'a, 'b, T, D> {}

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> TraceChunk<'a, 'b, T, D, Mut> {
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

    pub fn spans(&self) -> SpanIterator<'a, 'b, T, D, std::slice::Iter<'b, T::Span<'b>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }
}

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceChunk<'a, 'b, T, D, MUT> {
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

    pub fn spans_mut(&mut self) -> SpanIteratorMut<'a, 'b, T, D, std::slice::Iter<'b, T::Span<'b>>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }

    pub fn retain_spans<F: FnMut(&mut SpanMut<'a, 'b, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let chunk: &'a mut T::Chunk<'a> = as_mut(self.chunk);
            let storage_ref: &'a mut T::Storage<'a> = as_mut(self.storage);
            T::retain_spans(chunk, storage_ref, |span, storage| {
                let mut span_obj = Span::<'a, '_, T, D, MUT> { storage, span };
//                let span_ref: &mut Span<'a, T, D, MUT> = std::mem::transmute(&mut span_obj);
                predicate(&mut span_obj)
            })
        }
    }

    pub fn add_span(&mut self) -> Span<'a, 'b, T, D, MUT> {
        Span {
            storage: self.storage,
            span: unsafe { T::add_span(as_mut(self.chunk), as_mut(self.storage)) }
        }
    }
}

pub struct SpanIterator<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanIteratorMut<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>> = SpanIterator<'a, 'b, T, D, I, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::Span<'b>>, const Mut: u8> Iterator for SpanIterator<'a, 'b, T, D, I, Mut> {
    type Item = Span<'a, 'b, T, D, Mut>;

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
pub struct Span<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    span: &'b T::Span<'b>,
}
pub type SpanMut<'a: 'b, 'b, T, D> = Span<'a, 'b, T, D, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for Span<'a, 'b, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Span {
            storage: self.storage,
            span: self.span,
        }
    }
}
impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for Span<'a, 'b, T, D> {}

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> Span<'a, 'b, T, D, Mut> {
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

    pub fn span_links(&self) -> SpanLinkIterator<'a, 'b, T, D, std::slice::Iter<'b, T::SpanLink<'b>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn retain_span_links<F: FnMut(&mut SpanLinkMut<'a, 'b, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'b mut T::Span<'b> = as_mut(self.span);
            let storage_ref: &'a mut T::Storage<'a> = as_mut(self.storage);
            T::retain_span_links(span, storage_ref, |link, storage| {
                let mut link_obj = SpanLink::<'a, '_, T, D, MUT> { storage, link };
//                let link_ref: &mut SpanLink<'a, T, D, MUT> = std::mem::transmute(&mut link_obj);
                predicate(&mut link_obj)
            })
        }
    }

    pub fn add_span_link(&mut self) -> SpanLink<'a, 'b, T, D, MUT> {
        SpanLink {
            storage: self.storage,
            link: unsafe { T::add_span_link(as_mut(self.span), as_mut(self.storage)) }
        }
    }

    pub fn span_events(&self) -> SpanEventIterator<'a, 'b, T, D, std::slice::Iter<'b, T::SpanEvent<'b>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }

    pub fn retain_span_events<F: FnMut(&mut SpanEventMut<'a, 'b, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'b mut T::Span<'b> = as_mut(self.span);
            let storage_ref: &'a mut T::Storage<'a> = as_mut(self.storage);
            T::retain_span_events(span, storage_ref, |event, storage| {
                let mut event_obj = SpanEvent::<'a, '_, T, D, MUT> { storage, event };
                //let event_ref: &mut SpanEvent<'a, '_, T, D, MUT> = std::mem::transmute(&mut event_obj);
                predicate(&mut event_obj)
            })
        }
    }

    pub fn add_span_event(&mut self) -> SpanEvent<'a, 'b, T, D, MUT> {
        SpanEvent {
            storage: self.storage,
            event: unsafe { T::add_span_event(as_mut(self.span), as_mut(self.storage)) }
        }
    }
}

impl <'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanMut<'a, 'b, T, D> {
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

    pub fn span_links_mut(&mut self) -> SpanLinkIteratorMut<'a, 'b, T, D, std::slice::Iter<'b, T::SpanLink<'b>>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn span_events_mut(&mut self) -> SpanEventIteratorMut<'a, 'b, T, D, std::slice::Iter<'b, T::SpanEvent<'b>>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }
}

pub struct SpanLinkIterator<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanLinkIteratorMut<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>> = SpanLinkIterator<'a, 'b, T, D, I, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanLink<'b>>, const Mut: u8> Iterator for SpanLinkIterator<'a, 'b, T, D, I, Mut> {
    type Item = SpanLink<'a, 'b, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |link| {
            SpanLink {
                storage: self.storage,
                link,
            }
        })
    }
}

pub struct SpanEventIterator<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    it: I,
}
pub type SpanEventIteratorMut<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>> = SpanEventIterator<'a, 'b, T, D, I, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, I: Iterator<Item = &'b T::SpanEvent<'b>>, const Mut: u8> Iterator for SpanEventIterator<'a, 'b, T, D, I, Mut> {
    type Item = SpanEvent<'a, 'b, T, D>;

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
pub struct SpanLink<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    link: &'b T::SpanLink<'b>,
}
pub type SpanLinkMut<'a, 'b, T, D> = SpanLink<'a, 'b, T, D, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for SpanLink<'a, 'b, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanLink {
            storage: self.storage,
            link: self.link,
        }
    }
}
impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for SpanLink<'a, 'b, T, D> {}


impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> SpanLink<'a, 'b, T, D, Mut> {
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

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanLinkMut<'a, 'b, T, D> {
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
pub struct SpanEvent<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    event: &'b T::SpanEvent<'b>,
}
pub type SpanEventMut<'a: 'b, 'b, T, D> = SpanEvent<'a, 'b, T, D, MUT>;

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Clone for SpanEvent<'a, 'b, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanEvent {
            storage: self.storage,
            event: self.event,
        }
    }
}
impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> Copy for SpanEvent<'a, 'b, T, D> {}

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>, const Mut: u8> SpanEvent<'a, 'b, T, D, Mut> {
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

impl<'a: 'b, 'b, T: TraceProjector<D>, D: TraceDataLifetime<'a>> SpanEventMut<'a, 'b, T, D> {
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

pub struct AttributeArray<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: 'a, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage<'a>,
    container: C,
}
pub type AttributeArrayMut<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: 'a> = AttributeArray<'a, T, D, C, MUT>;

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: Clone> Clone for AttributeArray<'a, T, D, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        AttributeArray {
            storage: self.storage,
            container: self.container.clone(),
        }
    }
}
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C: Copy> Copy for AttributeArray<'a, T, D, C> {}

pub trait AttributeArrayOp<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: Sized + ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesOp<'storage, T, D, Self>>
{
    fn get_attribute_array_len(&self, storage: &T::Storage<'storage>) -> usize;
    fn get_attribute_array_value<'container>(&'container self, storage: &T::Storage<'storage>, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> AttributeArrayOp<'storage, T, D> for () {
    fn get_attribute_array_len(&self, _storage: &T::Storage<'storage>) -> usize {
        0
    }

    fn get_attribute_array_value<'container>(&'container self, _storage: &T::Storage<'storage>, _index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
        panic!("AttributeArrayOp::get_attribute_array_value called on empty array")
    }
}

pub trait AttributeArrayMutOp<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'storage, T, D> + ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesMutOp<'storage, T, D, Self>>
{
    fn get_attribute_array_value_mut<'container>(&'container mut self, storage: &mut T::Storage<'storage>, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>>;
    fn append_attribute_array_value<'container>(&'container mut self, storage: &mut T::Storage<'storage>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> AttributeArrayMutOp<'storage, T, D> for () {
    fn get_attribute_array_value_mut<'container>(&'container mut self, _storage: &mut T::Storage<'storage>, _index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, Self>> {
        None
    }

    fn append_attribute_array_value<'container>(&'container mut self, _storage: &mut T::Storage<'storage>, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
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

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C, const Mut: u8> AttributeArray<'storage, T, D, C, Mut>
where
    C: AttributeArrayOp<'storage, T, D>,
{
    fn len(&self) -> usize {
        self.container.get_attribute_array_len(self.storage)
    }

    fn get<'container>(&'container self, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<C>, C>, T, D, C> {
        self.container.get_attribute_array_value(self.storage, index)
    }
}

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C> AttributeArrayMut<'storage, T, D, C>
where
    C: AttributeArrayMutOp<'storage, T, D>,
{
    fn get_mut<'container>(&'container mut self, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C>> {
        unsafe { self.container.get_attribute_array_value_mut(as_mut(self.storage), index) }
    }

    fn append<'container>(&'container mut self, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<C>, C>, T, D, C> {
        unsafe { self.container.append_attribute_array_value(as_mut(self.storage), value) }
    }

    // TODO: retain_mut
}

// TODO MUT iter
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C, const Mut: u8> Iterator for AttributeArray<'a, T, D, C, Mut>
where
    TraceAttributes<'a, T, D, AttrOwned<C>, C, Mut>: TraceAttributesOp<'a, T, D, C>,
{
    type Item = AttributeAnyGetterContainer<'a, 'a, TraceAttributes<'a, T, D, AttrOwned<C>, C, Mut>, T, D, C>;

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

pub type AttributeAnyGetterContainer<'container, 'storage: 'container, A: TraceAttributesOp<'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    A::Array,
    A::Map,
>;

pub type AttributeAnySetterContainer<'container, 'storage: 'container, A: TraceAttributesMutOp<'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    A::MutString,
    A::MutBytes,
    A::MutBoolean,
    A::MutInteger,
    A::MutDouble,
    A::MutArray,
    A::MutMap,
>;

pub type AttributeAnyValue<'container, 'storage: 'container, A: TraceAttributesOp<'storage, T, D, C>, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    AttributeArray<'container, T, D, A::Array>,
    TraceAttributes<'container, T, D, AttrOwned<A::Map>, A::Map>,
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
pub trait ArrayAttributesOp<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>>: AttributeArrayOp<'a, T, D>
{}

pub trait MapAttributesOp<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> {}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C> ArrayAttributesOp<'a, T, D> for C
where
    C: AttributeArrayOp<'a, T, D>,
{}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C> MapAttributesOp<'a, T, D> for C {}

pub trait TraceAttributesOp<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C>
{
    type Array: ArrayAttributesOp<'storage, T, D>;
    type Map: MapAttributesOp<'storage, T, D>;

    fn get<'container, K>(container: &'container C, storage: &T::Storage<'storage>, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, C>>
    where
        'storage: 'container,
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;

    fn get_double<K>(container: &C, storage: &T::Storage<'storage>, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        Self::get(container, storage, key).and_then(|v| match v {
            AttributeAnyContainer::Double(d) => Some(d),
            _ => None,
        })
    }
}

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, const Mut: u8> TraceAttributesOp<'storage, T, D, ()> for TraceAttributes<'storage, T, D, AttrOwned<()>, (), Mut> {
    type Array = ();
    type Map = ();

    fn get<'container, K>(_container: &'container (), _storage: &T::Storage<'storage>, _key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        'storage: 'container,
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

// Helper traits to break the recursion cycle in TraceAttributesMutOp
pub trait ArrayAttributesMutOp<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>>: AttributeArrayMutOp<'a, T, D>
{}

pub trait MapAttributesMutOp<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> {}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C> ArrayAttributesMutOp<'a, T, D> for C
where
    C: AttributeArrayMutOp<'a, T, D>,
{}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>, C> MapAttributesMutOp<'a, T, D> for C {}

pub trait TraceAttributesMutOp<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C>: TraceAttributesOp<'storage, T, D, C>
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
    type MutArray: ArrayAttributesMutOp<'storage, T, D>;
    type MutMap: MapAttributesMutOp<'storage, T, D>;

    fn get_mut<'container, K>(container: &'container mut C, storage: &mut T::Storage<'storage>, key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>>
    where
        'storage: 'container,
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
    fn set<'container>(container: &'container mut C, storage: &mut T::Storage<'storage>, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, C> where 'storage: 'container;
    fn remove<K>(container: &mut C, storage: &mut T::Storage<'storage>, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
}

impl<'storage, T: TraceProjector<D>, D: TraceDataLifetime<'storage>> TraceAttributesMutOp<'storage, T, D, ()> for TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<'container, K>(_container: &'container mut (), _storage: &mut T::Storage<'storage>, _key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        'storage: 'container,
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }

    fn set<'container>(_container: &'container mut (), _storage: &mut T::Storage<'storage>, _key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()> where 'storage: 'container {
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
    fn get(&self, storage: &T::Storage<'a>) -> &'a D::Text;
    fn set(self, storage: &mut T::Storage<'a>, value: D::Text);
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceAttributesString<'a, T, D> for () {
    fn get(&self, _storage: &T::Storage<'a>) -> &'a D::Text {
        D::Text::default_ref()
    }

    fn set(self, _storage: &mut T::Storage<'a>, _value: D::Text) {
    }
}

pub trait TraceAttributesBytes<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> {
    fn get(&self, storage: &T::Storage<'a>) -> &'a D::Bytes;
    fn set(self, storage: &mut T::Storage<'a>, value: D::Bytes);
}

impl<'a, T: TraceProjector<D>, D: TraceDataLifetime<'a>> TraceAttributesBytes<'a, T, D> for () {
    fn get(&self, _storage: &T::Storage<'a>) -> &'a D::Bytes {
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
impl<'storage: 'container, 'container, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>: TraceAttributesOp<'storage, T, D, C>,
{
    pub fn get_double<K>(self, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        <TraceAttributes<'storage, T, D, AttrRef<'storage, C>, C> as TraceAttributesOp<'storage, T, D, C>>::get_double(self.container.0, self.storage, key)
    }
}

// Simplified mutable methods
impl<'storage: 'container, 'container, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>: TraceAttributesMutOp<'storage, T, D, C>,
{
    pub fn set_double<K: IntoData<D::Text>>(mut self, key: K, value: f64) {
        let AttributeAnyContainer::Double(container) = <Self as TraceAttributesMutOp<'storage, T, D, C>>::set(unsafe { self.container.as_mut() }, unsafe { as_mut(self.storage) }, key.into(), AttributeAnyValueType::Double) else { unreachable!() };
        container.set(value)
    }

    pub fn remove<K>(self, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        <Self as TraceAttributesMutOp<'storage, T, D, C>>::remove(unsafe { self.container.as_mut() }, unsafe { as_mut(self.storage) }, key);
    }
}

impl<'container, 'storage: 'container, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributes<'storage, T, D, V, C>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>: TraceAttributesOp<'storage, T, D, C>,
{
    fn fetch<K>(&self, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'storage, T, D, C>>::get(self.container.as_ref(), self.storage, key)
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
            }),
            AttributeAnyContainer::Map(map) => AttributeAnyValue::<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>, T, D, C>::Map(TraceAttributes {
                storage: self.storage,
                container: AttrOwned(map),
                _phantom: PhantomData,
            }),
        })
    }

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

    pub fn get_array<K>(&self, key: &K) -> Option<AttributeArray<'storage, T, D, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'storage, T, D, C>>::Array>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Array(container)) = self.fetch(key) {
            Some(AttributeArray {
                storage: self.storage,
                container,
            })
        } else {
            None
        }
    }


    pub fn get_map<K>(&self, key: &K) -> Option<TraceAttributes<'storage, T, D, AttrOwned<<TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'storage, T, D, C>>::Map>, <TraceAttributes<'storage, T, D, AttrRef<'container, C>, C> as TraceAttributesOp<'storage, T, D, C>>::Map>>
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

impl<'container, 'storage: 'container, T: TraceProjector<D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributesMut<'storage, T, D, V, C>
where
    D::Text: Clone + From<String> + for<'b> From<&'b str>,
    D::Bytes: Clone + From<Vec<u8>> + for<'b> From<&'b [u8]>,
    Self: TraceAttributesMutOp<'storage, T, D, C>,
{
    pub fn set_string<K: IntoData<D::Text>, Val: IntoData<D::Text>>(&mut self, key: K, value: Val) {
        let AttributeAnyContainer::String(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::String) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bytes<K: IntoData<D::Text>, Val: IntoData<D::Bytes>>(&mut self, key: K, value: Val) {
        let AttributeAnyContainer::Bytes(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Bytes) }) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    pub fn set_bool<K: IntoData<D::Text>>(&mut self, key: K, value: bool) {
        let AttributeAnyContainer::Boolean(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Boolean) }) else { unreachable!() };
        container.set(value)
    }

    pub fn set_empty_array<K: IntoData<D::Text>>(&mut self, key: K) -> AttributeArrayMut<'storage, T, D, <Self as TraceAttributesMutOp<'storage, T, D, C>>::MutArray> {
        let AttributeAnyContainer::Array(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Array) }) else { unreachable!() };
        AttributeArray {
            storage: self.storage,
            container,
        }
    }

    pub fn set_empty_map<K: IntoData<D::Text>>(&mut self, key: K) -> TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributesMutOp<'storage, T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<'storage, T, D, C>>::MutMap> {
        let AttributeAnyContainer::Map(container) = (unsafe { Self::set(self.container.as_mut(), as_mut(self.storage), key.into(), AttributeAnyValueType::Map) }) else { unreachable!() };
        TraceAttributes {
            storage: self.storage,
            container: AttrOwned(container),
            _phantom: PhantomData,
        }
    }

    pub fn get_array_mut<K>(&mut self, key: &K) -> Option<AttributeArrayMut<'storage, T, D, <Self as TraceAttributesMutOp<'storage, T, D, C>>::MutArray>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Array(container)) = unsafe { Self::get_mut(self.container.as_mut(), as_mut(self.storage), key) } {
            Some(AttributeArray {
                storage: self.storage,
                container,
            })
        } else {
            None
        }
    }


    pub fn get_map_mut<K>(&mut self, key: &K) -> Option<TraceAttributesMut<'storage, T, D, AttrOwned<<Self as TraceAttributesMutOp<'storage, T, D, C>>::MutMap>, <Self as TraceAttributesMutOp<'storage, T, D, C>>::MutMap>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        if let Some(AttributeAnyContainer::Map(container)) = unsafe { Self::get_mut(self.container.as_mut(), as_mut(self.storage), key) } {
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
