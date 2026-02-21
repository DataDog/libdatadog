use crate::span::{TraceDataLifetime, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{TraceAttributes, TraceAttributesMut, AttrRef};
use std::marker::PhantomData;

/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug)]
pub struct SpanLink<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    pub(crate) storage: &'s T::Storage,
    pub(crate) link: &'b T::SpanLink,
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

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'s, T, D, AttrRef<'b, T::SpanLink>, T::SpanLink> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.link),
            _phantom: PhantomData,
        }
    }
}
