use crate::span::{TraceDataLifetime, IntoData, OwnedTraceData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{TraceAttributes, TraceAttributesMut, AttrRef};
use super::{Span, SpanMut};
use std::marker::PhantomData;

/// A borrowed view over a single trace chunk.
///
/// A chunk groups the spans that share one sampling decision (priority, origin, trace ID).
/// Getter methods require no extra lifetime bound; methods returning references (e.g. `origin`)
/// require the chunk reference lifetime `'b` to outlive the storage lifetime `'s`.
///
/// [`TraceChunkMut`] is the mutable variant; it additionally exposes setter methods and
/// span mutation via `retain_spans` / `add_span`.
#[derive(Debug)]
pub struct TraceChunk<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    pub(super) storage: &'s T::Storage,
    pub(super) chunk: &'b T::Chunk,
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

    pub fn attributes_mut(&self) -> TraceAttributesMut<'s, T, D, AttrRef<'b, T::Chunk>, T::Chunk> {
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

/// Iterator over [`Span`] views within a [`TraceChunk`].
///
/// [`SpanIteratorMut`] is the mutable variant.
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
