use std::hash::Hash;
use std::marker::PhantomData;
use hashbrown::Equivalent;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{IntoData, OwnedTraceData, SpanDataContents, TraceDataLifetime, ImpliedPredicate};

pub trait TraceProjector<'s, D: TraceDataLifetime<'s>>: Sized + 's
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Trace>, Self::Trace>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Trace>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Chunk>, Self::Chunk>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Chunk>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::Span>, Self::Span>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::Span>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::SpanLink>, Self::SpanLink>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::SpanLink>>
    + for<'b> ImpliedPredicate<TraceAttributes<'s, Self, D, AttrRef<'b, Self::SpanEvent>, Self::SpanEvent>, Impls: TraceAttributesOp<'b, 's, Self, D, Self::SpanEvent>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Trace>, Self::Trace>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Trace>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Chunk>, Self::Chunk>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Chunk>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::Span>, Self::Span>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::Span>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::SpanLink>, Self::SpanLink>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::SpanLink>>
    + for<'b> ImpliedPredicate<TraceAttributesMut<'s, Self, D, AttrRef<'b, Self::SpanEvent>, Self::SpanEvent>, Impls: TraceAttributesMutOp<'b, 's, Self, D, Self::SpanEvent>>
{
    type Storage: 's;
    type Trace: 's;
    type Chunk: 's;
    type Span: 's;
    type SpanLink: 's;
    type SpanEvent: 's;

    fn project(&'s self) -> Traces<'s, Self, D>;
    fn project_mut(&'s mut self) -> TracesMut<'s, Self, D>;

    fn add_chunk<'b>(trace: &'b mut Self::Trace, storage: &mut Self::Storage) -> &'b mut Self::Chunk;
    fn chunk_iterator(trace: &'s Self::Trace) -> std::slice::Iter<'s, Self::Chunk>;
    fn retain_chunks<'b, F: for<'c> FnMut(&'c mut Self::Chunk, &'c mut Self::Storage) -> bool>(trace: &'b mut Self::Trace, storage: &'b mut Self::Storage, predicate: F);
    fn add_span<'b>(chunk: &'b mut Self::Chunk, storage: &mut Self::Storage) -> &'b mut Self::Span;
    fn span_iterator(chunk: &'s Self::Chunk) -> std::slice::Iter<'s, Self::Span>;
    fn retain_spans<'b, F: FnMut(&mut Self::Span, &mut Self::Storage) -> bool>(chunk: &'b mut Self::Chunk, storage: &'b mut Self::Storage, predicate: F);
    fn add_span_link<'b>(span: &'b mut Self::Span, storage: &mut Self::Storage) -> &'b mut Self::SpanLink;
    fn span_link_iterator(span: &'s Self::Span) -> std::slice::Iter<'s, Self::SpanLink>;
    fn retain_span_links<'b, F: FnMut(&mut Self::SpanLink, &mut Self::Storage) -> bool>(span: &'b mut Self::Span, storage: &'b mut Self::Storage, predicate: F);
    fn add_span_event<'b>(span: &'b mut Self::Span, storage: &mut Self::Storage) -> &'b mut Self::SpanEvent;
    fn span_event_iterator(span: &'s Self::Span) -> std::slice::Iter<'s, Self::SpanEvent>;
    fn retain_span_events<'b, F: FnMut(&mut Self::SpanEvent, &mut Self::Storage) -> bool>(span: &'b mut Self::Span, storage: &'b mut Self::Storage, predicate: F);

    fn get_trace_container_id(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_language_name(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_language_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_tracer_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_runtime_id(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_env(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_hostname(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_trace_app_version(trace: &'s Self::Trace, storage: &'s Self::Storage) -> &'s D::Text;

    fn set_trace_container_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_language_name(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_language_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_tracer_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_runtime_id(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_env(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_hostname(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);
    fn set_trace_app_version(trace: &mut Self::Trace, storage: &mut Self::Storage, value: D::Text);

    fn get_chunk_priority<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> i32;
    fn get_chunk_origin(chunk: &'s Self::Chunk, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_chunk_dropped_trace<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> bool;
    fn get_chunk_trace_id<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> u128;
    fn get_chunk_sampling_mechanism<'a>(chunk: &'a Self::Chunk, storage: &'a Self::Storage) -> u32;

    fn set_chunk_priority(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: i32);
    fn set_chunk_origin(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: D::Text);
    fn set_chunk_dropped_trace(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: bool);
    fn set_chunk_trace_id(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u128) where D: OwnedTraceData;
    fn set_chunk_sampling_mechanism(chunk: &mut Self::Chunk, storage: &mut Self::Storage, value: u32);

    fn get_span_service(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_name(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_resource(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_type(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_span_id(span: &'s Self::Span, storage: &'s Self::Storage) -> u64;
    fn get_span_parent_id(span: &'s Self::Span, storage: &'s Self::Storage) -> u64;
    fn get_span_start(span: &'s Self::Span, storage: &'s Self::Storage) -> i64;
    fn get_span_duration(span: &'s Self::Span, storage: &'s Self::Storage) -> i64;
    fn get_span_error(span: &'s Self::Span, storage: &'s Self::Storage) -> bool;
    fn get_span_env(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_version(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_component(span: &'s Self::Span, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_span_kind(span: &'s Self::Span, storage: &'s Self::Storage) -> SpanKind;

    fn set_span_service(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_name(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_resource(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_type(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_span_id(span: &mut Self::Span, storage: &mut Self::Storage, value: u64);
    fn set_span_parent_id(span: &mut Self::Span, storage: &mut Self::Storage, value: u64);
    fn set_span_start(span: &mut Self::Span, storage: &mut Self::Storage, value: i64);
    fn set_span_duration(span: &mut Self::Span, storage: &mut Self::Storage, value: i64);
    fn set_span_error(span: &mut Self::Span, storage: &mut Self::Storage, value: bool);
    fn set_span_env(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_version(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_component(span: &mut Self::Span, storage: &mut Self::Storage, value: D::Text);
    fn set_span_kind(span: &mut Self::Span, storage: &mut Self::Storage, value: SpanKind);

    fn get_link_trace_id(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u128;
    fn get_link_span_id(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u64;
    fn get_link_trace_state(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> &'s D::Text;
    fn get_link_flags(link: &'s Self::SpanLink, storage: &'s Self::Storage) -> u32;

    fn set_link_trace_id(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u128);
    fn set_link_span_id(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u64);
    fn set_link_trace_state(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: D::Text);
    fn set_link_flags(link: &mut Self::SpanLink, storage: &mut Self::Storage, value: u32);

    fn get_event_time_unix_nano(event: &'s Self::SpanEvent, storage: &'s Self::Storage) -> u64;
    fn get_event_name(event: &'s Self::SpanEvent, storage: &'s Self::Storage) -> &'s D::Text;

    fn set_event_time_unix_nano(event: &mut Self::SpanEvent, storage: &mut Self::Storage, value: u64);
    fn set_event_name(event: &mut Self::SpanEvent, storage: &mut Self::Storage, value: D::Text);
}

pub const IMMUT: u8 = 0;
pub const MUT: u8 = 1;

#[allow(invalid_reference_casting)]
unsafe fn as_mut<T>(v: &T) -> &mut T {
    &mut *(v as *const _ as *mut _)
}

struct TraceValue<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C, const TYPE: u8, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    container: &'s C,
}

#[derive(Debug)]
pub struct Traces<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    traces: &'s T::Trace,
}
pub type TracesMut<'s, T, D> = Traces<'s, T, D, MUT>;

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Clone for Traces<'s, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Traces {
            storage: self.storage,
            traces: self.traces,
        }
    }
}
impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Copy for Traces<'s, T, D> {}

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Traces<'s, T, D> {
    pub fn new(traces: &'s T::Trace, storage: &'s T::Storage) -> Self {
        Self::generic_new(traces, storage)
    }
}

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> Traces<'s, T, D, ISMUT> {
    fn generic_new(traces: &'s T::Trace, storage: &'s T::Storage) -> Self {
        Traces {
            storage,
            traces,
        }
    }

    pub fn container_id(&self) -> &'s D::Text {
        T::get_trace_container_id(self.traces, self.storage)
    }

    pub fn language_name(&self) -> &'s D::Text {
        T::get_trace_language_name(self.traces, self.storage)
    }

    pub fn language_version(&self) -> &'s D::Text {
        T::get_trace_language_version(self.traces, self.storage)
    }

    pub fn tracer_version(&self) -> &'s D::Text {
        T::get_trace_tracer_version(self.traces, self.storage)
    }

    pub fn runtime_id(&self) -> &'s D::Text {
        T::get_trace_runtime_id(self.traces, self.storage)
    }

    pub fn env(&self) -> &'s D::Text {
        T::get_trace_env(self.traces, self.storage)
    }

    pub fn hostname(&self) -> &'s D::Text {
        T::get_trace_hostname(self.traces, self.storage)
    }

    pub fn app_version(&self) -> &'s D::Text {
        T::get_trace_app_version(self.traces, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'s, T, D, AttrRef<'s, T::Trace>, T::Trace> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.traces),
            _phantom: PhantomData,
        }
    }

    pub fn chunks(&self) -> ChunkIterator<'s, 's, T, D, std::slice::Iter<'s, T::Chunk>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }
}

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TracesMut<'s, T, D> {
    pub fn new_mut(traces: &'s mut T::Trace, storage: &'s mut T::Storage) -> Self {
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

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'s, T, D, AttrRef<'s, T::Trace>, T::Trace> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.traces),
            _phantom: PhantomData,
        }
    }

    pub fn chunks_mut(&mut self) -> ChunkIteratorMut<'s, 's, T, D, std::slice::Iter<'s, T::Chunk>> {
        ChunkIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.traces)
        }
    }

    pub fn retain_chunks<F: FnMut(&mut TraceChunkMut<'s, 's, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let traces = as_mut(self.traces);
            let storage_ref: &'s mut T::Storage = as_mut(self.storage);
            T::retain_chunks(traces, storage_ref, move |chunk, storage| {
                let mut trace_chunk = TraceChunkMut {
                    storage: std::mem::transmute::<&mut T::Storage, &'s mut T::Storage>(storage),
                    chunk: std::mem::transmute::<&mut T::Chunk, &'s mut T::Chunk>(chunk)
                };
                predicate(&mut trace_chunk)
            })
        }
    }

    pub fn add_chunk(&mut self) -> TraceChunkMut<'_, 's, T, D> {
        TraceChunk {
            storage: self.storage,
            chunk: unsafe { T::add_chunk(as_mut(self.traces), as_mut(self.storage)) },
        }
    }
}

pub struct ChunkIterator<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::Chunk>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    it: I,
}
pub type ChunkIteratorMut<'b, 's, T, D, I> = ChunkIterator<'b, 's, T, D, I, MUT>;

impl<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::Chunk>, const ISMUT: u8> Iterator for ChunkIterator<'b, 's, T, D, I, ISMUT> {
    type Item = TraceChunk<'b, 's, T, D, ISMUT>;

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
pub struct TraceChunk<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    chunk: &'b T::Chunk,
}
pub type TraceChunkMut<'b, 's, T, D> = TraceChunk<'b, 's, T, D, MUT>;

impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Clone for TraceChunk<'b, 's, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceChunk {
            storage: self.storage,
            chunk: self.chunk,
        }
    }
}
impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Copy for TraceChunk<'b, 's, T, D> {}

// Methods that don't need 'b: 's bound (return non-references)
impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> TraceChunk<'b, 's, T, D, ISMUT>  {
    pub fn priority(&self) -> i32 {
        T::get_chunk_priority(self.chunk, self.storage)
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
}

// Methods that need 'b: 's bound (return references with lifetimes)
impl<'b: 's, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> TraceChunk<'b, 's, T, D, ISMUT>  {
    pub fn origin(&self) -> &'s D::Text {
        T::get_chunk_origin(self.chunk, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::Chunk>, T::Chunk> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans(&self) -> SpanIterator<'b, 's, T, D, std::slice::Iter<'b, T::Span>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }
}

impl<'b: 's, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TraceChunk<'b, 's, T, D, MUT>  {
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

    pub fn attributes_mut(&self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::Chunk>, T::Chunk, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.chunk),
            _phantom: PhantomData,
        }
    }

    pub fn spans_mut(&mut self) -> SpanIteratorMut<'b, 's, T, D, std::slice::Iter<'b, T::Span>> {
        SpanIterator {
            storage: self.storage,
            it: T::span_iterator(self.chunk)
        }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn retain_spans<F: for<'r> FnMut(&mut SpanMut<'r, 's, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let chunk: &'s mut T::Chunk = std::mem::transmute(self.chunk);
            let storage_ref: &'s mut T::Storage = std::mem::transmute(self.storage);
            T::retain_spans(chunk, storage_ref, |span, storage| {
                let span_ref: &'s mut T::Span = std::mem::transmute(span);
                let storage_ref: &'s mut T::Storage = std::mem::transmute(storage);
                let mut span_obj = Span::<'_, 's, T, D, MUT> { storage: storage_ref, span: span_ref };
                predicate(&mut span_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span(&mut self) -> Span<'_, 's, T, D, MUT> {
        unsafe {
            let chunk: &'s mut T::Chunk = std::mem::transmute(self.chunk);
            let storage_transmuted: &mut T::Storage = std::mem::transmute(self.storage);
            let span_ref = T::add_span(chunk, storage_transmuted);
            Span {
                storage: self.storage,
                span: std::mem::transmute(span_ref)
            }
        }
    }
}

pub struct SpanIterator<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::Span>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    it: I,
}
pub type SpanIteratorMut<'b, 's, T, D, I> = SpanIterator<'b, 's, T, D, I, MUT>;

impl<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::Span>, const ISMUT: u8> Iterator for SpanIterator<'b, 's, T, D, I, ISMUT> {
    type Item = Span<'b, 's, T, D, ISMUT>;

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
pub struct Span<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    span: &'b T::Span,
}
pub type SpanMut<'b, 's, T, D> = Span<'b, 's, T, D, MUT>;

impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Clone for Span<'b, 's, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        Span {
            storage: self.storage,
            span: self.span,
        }
    }
}
impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Copy for Span<'b, 's, T, D> {}

impl<'b: 's, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> Span<'b, 's, T, D, ISMUT>  {
    pub fn service(&self) -> &'s D::Text {
        T::get_span_service(self.span, self.storage)
    }

    pub fn name(&self) -> &'s D::Text {
        T::get_span_name(self.span, self.storage)
    }

    pub fn resource(&self) -> &'s D::Text {
        T::get_span_resource(self.span, self.storage)
    }

    pub fn r#type(&self) -> &'s D::Text {
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

    pub fn env(&self) -> &'s D::Text {
        T::get_span_env(self.span, self.storage)
    }

    pub fn version(&self) -> &'s D::Text {
        T::get_span_version(self.span, self.storage)
    }

    pub fn component(&self) -> &'s D::Text {
        T::get_span_component(self.span, self.storage)
    }

    pub fn kind(&self) -> SpanKind {
        T::get_span_kind(self.span, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::Span>, T::Span> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links(&self) -> SpanLinkIterator<'b, 's, T, D, std::slice::Iter<'b, T::SpanLink>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    #[allow(mutable_transmutes)]
    pub fn retain_span_links<F: for<'r> FnMut(&mut SpanLinkMut<'r, 's, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'s mut T::Span = std::mem::transmute(self.span);
            let storage_ref: &'s mut T::Storage = std::mem::transmute(self.storage);
            T::retain_span_links(span, storage_ref, |link, storage| {
                let link_ref: &'s mut T::SpanLink = std::mem::transmute(link);
                let storage_ref: &'s mut T::Storage = std::mem::transmute(storage);
                let mut link_obj = SpanLink::<'_, 's, T, D, MUT> { storage: storage_ref, link: link_ref };
                predicate(&mut link_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span_link(&mut self) -> SpanLink<'_, 's, T, D, MUT> {
        unsafe {
            let span: &'s mut T::Span = std::mem::transmute(self.span);
            let storage_transmuted: &mut T::Storage = std::mem::transmute(self.storage);
            let link_ref = T::add_span_link(span, storage_transmuted);
            SpanLink {
                storage: self.storage,
                link: std::mem::transmute(link_ref)
            }
        }
    }

    pub fn span_events(&self) -> SpanEventIterator<'b, 's, T, D, std::slice::Iter<'b, T::SpanEvent>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }

    #[allow(mutable_transmutes)]
    pub fn retain_span_events<F: for<'r> FnMut(&mut SpanEventMut<'r, 's, T, D>) -> bool>(&mut self, mut predicate: F) {
        // We may not make self.storage mut inside the closure. As that would be a double mut-borrow
        unsafe {
            let span: &'s mut T::Span = std::mem::transmute(self.span);
            let storage_ref: &'s mut T::Storage = std::mem::transmute(self.storage);
            T::retain_span_events(span, storage_ref, |event, storage| {
                let event_ref: &'s mut T::SpanEvent = std::mem::transmute(event);
                let storage_ref: &'s mut T::Storage = std::mem::transmute(storage);
                let mut event_obj = SpanEvent::<'_, 's, T, D, MUT> { storage: storage_ref, event: event_ref };
                predicate(&mut event_obj)
            })
        }
    }

    #[allow(mutable_transmutes)]
    pub fn add_span_event(&mut self) -> SpanEvent<'b, 's, T, D, MUT> {
        unsafe {
            let span: &mut T::Span = std::mem::transmute(self.span);
            let storage_transmuted: &mut T::Storage = std::mem::transmute(self.storage);
            let event_ref = T::add_span_event(span, storage_transmuted);
            SpanEvent {
                storage: self.storage,
                event: std::mem::transmute(event_ref)
            }
        }
    }
}

impl <'b: 's, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> SpanMut<'b, 's, T, D>  {
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

    pub fn attributes_mut(&mut self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::Span>, T::Span, MUT> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.span),
            _phantom: PhantomData,
        }
    }

    pub fn span_links_mut(&mut self) -> SpanLinkIteratorMut<'b, 's, T, D, std::slice::Iter<'b, T::SpanLink>> {
        SpanLinkIterator {
            storage: self.storage,
            it: T::span_link_iterator(self.span)
        }
    }

    pub fn span_events_mut(&mut self) -> SpanEventIteratorMut<'b, 's, T, D, std::slice::Iter<'b, T::SpanEvent>> {
        SpanEventIterator {
            storage: self.storage,
            it: T::span_event_iterator(self.span)
        }
    }
}

pub struct SpanLinkIterator<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::SpanLink>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    it: I,
}
pub type SpanLinkIteratorMut<'b, 's, T, D, I> = SpanLinkIterator<'b, 's, T, D, I, MUT>;

impl<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::SpanLink>, const ISMUT: u8> Iterator for SpanLinkIterator<'b, 's, T, D, I, ISMUT> {
    type Item = SpanLink<'b, 's, T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |link| {
            SpanLink {
                storage: self.storage,
                link,
            }
        })
    }
}

pub struct SpanEventIterator<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::SpanEvent>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    it: I,
}
pub type SpanEventIteratorMut<'b, 's, T, D, I> = SpanEventIterator<'b, 's, T, D, I, MUT>;

impl<'b, 's: 'b, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, I: Iterator<Item = &'b T::SpanEvent>, const ISMUT: u8> Iterator for SpanEventIterator<'b, 's, T, D, I, ISMUT> {
    type Item = SpanEvent<'b, 's, T, D>;

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
pub struct SpanLink<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    link: &'b T::SpanLink,
}
pub type SpanLinkMut<'b, 's, T, D> = SpanLink<'b, 's, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Clone for SpanLink<'b, 's, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanLink {
            storage: self.storage,
            link: self.link,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Copy for SpanLink<'b, 's, T, D> {}


impl<'b: 's, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> SpanLink<'b, 's, T, D, ISMUT>  {
    pub fn trace_id(&self) -> u128 {
        T::get_link_trace_id(self.link, self.storage)
    }

    pub fn span_id(&self) -> u64 {
        T::get_link_span_id(self.link, self.storage)
    }

    pub fn trace_state(&self) -> &'s D::Text {
        T::get_link_trace_state(self.link, self.storage)
    }

    pub fn flags(&self) -> u32 {
        T::get_link_flags(self.link, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::SpanLink>, T::SpanLink> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.link),
            _phantom: PhantomData,
        }
    }
}

impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> SpanLinkMut<'b, 's, T, D>  {
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

    pub fn attributes_mut(&mut self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::SpanLink>, T::SpanLink, MUT> {
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
pub struct SpanEvent<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    event: &'b T::SpanEvent,
}
pub type SpanEventMut<'b, 's, T, D> = SpanEvent<'b, 's, T, D, MUT>;

impl<'b, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Clone for SpanEvent<'b, 's, T, D> { // Note: not for MUT
    fn clone(&self) -> Self {
        SpanEvent {
            storage: self.storage,
            event: self.event,
        }
    }
}
impl<'b, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> Copy for SpanEvent<'b, 's, T, D> {}

impl<'b: 's, 's, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8> SpanEvent<'b, 's, T, D, ISMUT>  {
    pub fn time_unix_nano(&self) -> u64 {
        T::get_event_time_unix_nano(self.event, self.storage)
    }

    pub fn name(&self) -> &'s D::Text {
        T::get_event_name(self.event, self.storage)
    }

    pub fn attributes(&self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::SpanEvent>, T::SpanEvent> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.event),
            _phantom: PhantomData,
        }
    }
}

impl<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> SpanEventMut<'b, 's, T, D>  {
    pub fn set_time_unix_nano(&mut self, value: u64) {
        unsafe { T::set_event_time_unix_nano(as_mut(self.event), as_mut(self.storage), value) }
    }

    pub fn set_name<I: IntoData<D::Text>>(&mut self, value: I) {
        unsafe { T::set_event_name(as_mut(self.event), as_mut(self.storage), value.into()) }
    }

    pub fn attributes_mut(&mut self) -> TraceAttributes<'s, T, D, AttrRef<'b, T::SpanEvent>, T::SpanEvent, MUT> {
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

pub struct AttributeArray<'c, 's: 'c, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: 'c, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    container: C,
    _phantom: PhantomData<&'c C>,
}
#[allow(type_alias_bounds)]
pub type AttributeArrayMut<'c, 's: 'c, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: 'c> = AttributeArray<'c, 's, T, D, C, MUT>;

impl<'s: 'c, 'c, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: Clone> Clone for AttributeArray<'c, 's, T, D, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        AttributeArray {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'s: 'c, 'c, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, C: Copy> Copy for AttributeArray<'c, 's, T, D, C> {}

pub trait AttributeArrayOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: Sized + ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesOp<'container, 'storage, T, D, Self>> + 'container
{
    fn get_attribute_array_len(&self, storage: &T::Storage) -> usize;
    fn get_attribute_array_value(&'container self, storage: &T::Storage, index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> AttributeArrayOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_len(&self, _storage: &T::Storage) -> usize {
        0
    }

    fn get_attribute_array_value(&self, _storage: &T::Storage, _index: usize) -> AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
        panic!("AttributeArrayOp::get_attribute_array_value called on empty array")
    }
}

pub trait AttributeArrayMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D> + ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self>> + 'container
{
    fn get_attribute_array_value_mut(&'container mut self, storage: &mut T::Storage, index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>>;
    fn append_attribute_array_value(&'container mut self, storage: &mut T::Storage, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>, T, D, Self>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> AttributeArrayMutOp<'container, 'storage, T, D> for () {
    fn get_attribute_array_value_mut(&'container mut self, _storage: &mut T::Storage, _index: usize) -> Option<AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, Self>> {
        None
    }

    fn append_attribute_array_value(&'container mut self, _storage: &mut T::Storage, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()>, T, D, ()> {
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

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container, const ISMUT: u8> AttributeArray<'container, 'storage, T, D, C, ISMUT>
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

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> AttributeArrayMut<'container, 'storage, T, D, C>
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
impl<'storage, 'container, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container, const ISMUT: u8> Iterator for AttributeArray<'container, 'storage, T, D, C, ISMUT>
where
    TraceAttributes<'storage, T, D, AttrOwned<C>, C, ISMUT>: TraceAttributesOp<'container, 'storage, T, D, C>,
{
    type Item = AttributeAnyGetterContainer<'container, 'storage, TraceAttributes<'storage, T, D, AttrOwned<C>, C, ISMUT>, T, D, C>;

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

#[allow(type_alias_bounds)]
pub type AttributeAnyGetterContainer<'container, 'storage, A: TraceAttributesOp<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    A::Array,
    A::Map,
>;

#[allow(type_alias_bounds)]
pub type AttributeAnySetterContainer<'container, 'storage, A: TraceAttributesMutOp<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    A::MutString,
    A::MutBytes,
    A::MutBoolean,
    A::MutInteger,
    A::MutDouble,
    A::MutArray,
    A::MutMap,
>;

#[allow(type_alias_bounds)]
pub type AttributeAnyValue<'container, 'storage, A: TraceAttributesOp<'container, 'storage, T, D, C>, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> = AttributeAnyContainer<
    &'storage D::Text,
    &'storage D::Bytes,
    bool,
    i64,
    f64,
    AttributeArray<'container, 'storage, T, D, A::Array>,
    TraceAttributes<'storage, T, D, AttrOwned<A::Map>, A::Map>,
>;

pub trait AttrVal<C> {
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

pub struct TraceAttributes<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, V: AttrVal<C>, C, const ISMUT: u8 = IMMUT> {
    storage: &'s T::Storage,
    container: V,
    _phantom: PhantomData<C>,
}
pub type TraceAttributesMut<'s, T, D, V, C> = TraceAttributes<'s, T, D, V, C, MUT>;

impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, V: AttrVal<C> + Clone, C> Clone for TraceAttributes<'s, T, D, V, C> { // Note: not for MUT
    fn clone(&self) -> Self {
        TraceAttributes {
            storage: self.storage,
            container: self.container.clone(),
            _phantom: PhantomData,
        }
    }
}
impl<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, A: AttrVal<C> + Copy, C> Copy for TraceAttributes<'s, T, D, A, C> {}

// Helper traits to break the recursion cycle in TraceAttributesOp
pub trait ArrayAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayOp<'container, 'storage, T, D>
{}

pub trait MapAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributes<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesOp<'container, 'storage, T, D> for C
where
    TraceAttributes<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

pub trait TraceAttributesOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container>
{
    type Array: ArrayAttributesOp<'container, 'storage, T, D>;
    type Map;

    fn get<K>(container: &'container C, storage: &'storage T::Storage, key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;

    fn get_double<K>(container: &'container C, storage: &'storage T::Storage, key: &K) -> Option<f64>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        Self::get(container, storage, key).and_then(|v| match v {
            AttributeAnyContainer::Double(d) => Some(d),
            _ => None,
        })
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, const ISMUT: u8> TraceAttributesOp<'container, 'storage, T, D, ()> for TraceAttributes<'storage, T, D, AttrOwned<()>, (), ISMUT> {
    type Array = ();
    type Map = ();

    fn get<K>(_container: &'container (), _storage: &'storage T::Storage, _key: &K) -> Option<AttributeAnyGetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        None
    }
}

// Helper traits to break the recursion cycle in TraceAttributesMutOp
pub trait ArrayAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: AttributeArrayMutOp<'container, 'storage, T, D>
{}

pub trait MapAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>>: ImpliedPredicate<TraceAttributesMut<'storage, T, D, AttrOwned<Self::Container>, Self::Container>, Impls: TraceAttributesMutOp<'container, 'storage, T, D, Self::Container>> {
    type Container: 'container;
}

// Blanket implementations - any type implementing the base trait gets the helper trait
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C> ArrayAttributesMutOp<'container, 'storage, T, D> for C
where
    C: AttributeArrayMutOp<'container, 'storage, T, D> + 'container,
{}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> MapAttributesMutOp<'container, 'storage, T, D> for C
where
    TraceAttributesMut<'storage, T, D, AttrOwned<Self>, Self>: TraceAttributesMutOp<'container, 'storage, T, D, Self>
{
    type Container = Self;
}

pub trait TraceAttributesMutOp<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container>: TraceAttributesOp<'container, 'storage, T, D, C>
where
    Self::MutString: TraceAttributesString<'storage, 'storage, T, D>,
    Self::MutBytes: TraceAttributesBytes<'storage, 'storage, T, D>,
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

    fn get_mut<K>(container: &'container mut C, storage: &mut T::Storage, key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
    fn set(container: &'container mut C, storage: &mut T::Storage, key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, C>;
    fn remove<K>(container: &mut C, storage: &mut T::Storage, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>;
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>> TraceAttributesMutOp<'container, 'storage, T, D, ()> for TraceAttributesMut<'storage, T, D, AttrOwned<()>, ()> {
    type MutString = ();
    type MutBytes = ();
    type MutBoolean = ();
    type MutInteger = ();
    type MutDouble = ();
    type MutArray = ();
    type MutMap = ();

    fn get_mut<K>(_container: &'container mut (), _storage: &mut T::Storage, _key: &K) -> Option<AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()>>
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>,
    {
        None
    }

    fn set(_container: &'container mut (), _storage: &mut T::Storage, _key: D::Text, value: AttributeAnyValueType) -> AttributeAnySetterContainer<'container, 'storage, Self, T, D, ()> {
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

    fn remove<K>(_container: &mut (), _storage: &mut T::Storage, _key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
    }
}

pub trait TraceAttributesString<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> {
    fn get(&self, storage: &'a T::Storage) -> &'s D::Text;
    fn set(self, storage: &mut T::Storage, value: D::Text);
}

impl<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TraceAttributesString<'s, 'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage) -> &'s D::Text {
        D::Text::default_ref()
    }

    fn set(self, _storage: &mut T::Storage, _value: D::Text) {
    }
}

pub trait TraceAttributesBytes<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> {
    fn get(&self, storage: &'a T::Storage) -> &'a D::Bytes;
    fn set(self, storage: &mut T::Storage, value: D::Bytes);
}

impl<'s, 'a, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>> TraceAttributesBytes<'s, 'a, T, D> for () {
    fn get(&self, _storage: &'a T::Storage) -> &'a D::Bytes {
        D::Bytes::default_ref()
    }

    fn set(self, _storage: &mut T::Storage, _value: D::Bytes) {
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
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C>
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
impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, C: 'container> TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>
where
    TraceAttributes<'storage, T, D, AttrRef<'container, C>, C, MUT>: TraceAttributesMutOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_double<K: IntoData<D::Text>>(&mut self, key: K, value: f64) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Double(container) = <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Double) else { unreachable!() };
        container.set(value)
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn remove<K>(&mut self, key: &K)
    where
        K: ?Sized + Hash + Equivalent<<D::Text as SpanDataContents>::RefCopy>
    {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
        <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::remove(container_ref, storage_ref, key);
    }
}

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributes<'storage, T, D, V, C>
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

impl<'container, 'storage, T: TraceProjector<'storage, D>, D: TraceDataLifetime<'storage>, V: AttrVal<C>, C: 'container> TraceAttributesMut<'storage, T, D, V, C>
where
    D::Text: Clone + From<String> + for<'b> From<&'b str>,
    D::Bytes: Clone + From<Vec<u8>> + for<'b> From<&'b [u8]>,
    Self: TraceAttributesMutOp<'container, 'storage, T, D, C>,
{
    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_string<K: IntoData<D::Text>, Val: IntoData<D::Text>>(&mut self, key: K, value: Val) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::String(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::String) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_bytes<K: IntoData<D::Text>, Val: IntoData<D::Bytes>>(&mut self, key: K, value: Val) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Bytes(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Bytes) else { unreachable!() };
        unsafe { container.set(as_mut(self.storage), value.into()) }
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_bool<K: IntoData<D::Text>>(&mut self, key: K, value: bool) {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
        let AttributeAnyContainer::Boolean(container) = Self::set(container_ref, storage_ref, key.into(), AttributeAnyValueType::Boolean) else { unreachable!() };
        container.set(value)
    }

    #[allow(invalid_reference_casting, mutable_transmutes)]
    pub fn set_empty_array<K: IntoData<D::Text>>(&mut self, key: K) -> AttributeArrayMut<'container, 'storage, T, D, <Self as TraceAttributesMutOp<'container, 'storage, T, D, C>>::MutArray> {
        let container_ref: &'container mut C = unsafe { &mut *(self.container.as_ref() as *const C as *mut C) };
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
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
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
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
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
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
        let storage_ref: &mut T::Storage = unsafe { as_mut(self.storage) };
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
