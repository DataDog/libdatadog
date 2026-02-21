use std::marker::PhantomData;
use crate::span::{TraceDataLifetime, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{TraceAttributes, TraceAttributesMut, AttrRef};
use super::{TraceChunk, TraceChunkMut};

/// A borrowed view over the top-level trace container.
///
/// Exposes trace-wide metadata fields (container ID, language, runtime ID, …) and provides
/// an iterator over the [`TraceChunk`]s it contains.
///
/// The const `ISMUT` parameter selects between the read-only variant and [`TracesMut`],
/// which additionally exposes setter methods and chunk mutation.
#[derive(Debug)]
pub struct Traces<'s, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    pub(super) storage: &'s T::Storage,
    pub(super) traces: &'s T::Trace,
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

/// Iterator over [`TraceChunk`] views within a [`Traces`].
///
/// Yielded items share the storage lifetime `'s` of the parent trace container.
/// [`ChunkIteratorMut`] is the mutable variant.
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
