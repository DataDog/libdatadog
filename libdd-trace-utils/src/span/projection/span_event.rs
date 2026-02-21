use crate::span::{TraceDataLifetime, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{TraceAttributes, TraceAttributesMut, AttrRef};
use std::marker::PhantomData;

/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug)]
pub struct SpanEvent<'b, 's, T: TraceProjector<'s, D>, D: TraceDataLifetime<'s>, const ISMUT: u8 = IMMUT> {
    pub(crate) storage: &'s T::Storage,
    pub(crate) event: &'b T::SpanEvent,
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

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'s, T, D, AttrRef<'b, T::SpanEvent>, T::SpanEvent> {
        TraceAttributes {
            storage: self.storage,
            container: AttrRef(self.event),
            _phantom: PhantomData,
        }
    }
}
