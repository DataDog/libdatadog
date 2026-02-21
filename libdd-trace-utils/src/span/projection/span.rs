use std::marker::PhantomData;
use libdd_trace_protobuf::pb::idx::SpanKind;
use crate::span::{TraceDataLifetime, IntoData};
use super::{TraceProjector, IMMUT, MUT, as_mut};
use super::{TraceAttributes, TraceAttributesMut, AttrRef};
use super::{SpanLink, SpanLinkMut};
use super::{SpanEvent, SpanEventMut};

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
    pub(crate) storage: &'s T::Storage,
    pub(crate) span: &'b T::Span,
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

    pub fn attributes_mut(&mut self) -> TraceAttributesMut<'s, T, D, AttrRef<'b, T::Span>, T::Span> {
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
    type Item = SpanLink<'b, 's, T, D, ISMUT>;

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
    type Item = SpanEvent<'b, 's, T, D, ISMUT>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(move |event| {
            SpanEvent {
                storage: self.storage,
                event,
            }
        })
    }
}
