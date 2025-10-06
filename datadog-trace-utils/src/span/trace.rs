use std::marker::PhantomData;
use datadog_trace_protobuf::pb::idx::SpanKind;
use crate::span::TraceData;

pub trait TraceProjector<D: TraceData>
where
    AttributeArray<Self, D>: AttributeArrayOp<Self, D>,
    TraceAttributes<Self, D, Self::TraceRef>: TraceAttributesOp<Self, D>,
    TraceAttributes<Self, D, Self::ChunkRef>: TraceAttributesOp<Self, D>,
    TraceAttributes<Self, D, Self::SpanRef>: TraceAttributesOp<Self, D>,
    TraceValue<Self, D, { TraceValueType::ContainerId as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::LanguageName as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::LanguageVersion as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::TracerVersion as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::RuntimeId as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::Env as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::Hostname as u8 }>: TraceValueOp<D>,
    TraceValue<Self, D, { TraceValueType::AppVersion as u8 }>: TraceValueOp<D>,
    ChunkValue<Self, D, { ChunkValueType::Priority as u8 }>: TraceValueOp<D>,
    ChunkValue<Self, D, { ChunkValueType::Origin as u8 }>: TraceValueOp<D>,
    ChunkValue<Self, D, { ChunkValueType::DroppedTrace as u8 }>: TraceValueOp<D>,
    ChunkValue<Self, D, { ChunkValueType::TraceId as u8 }>: TraceValueOp<D>,
    ChunkValue<Self, D, { ChunkValueType::SamplingMechanism as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Service as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Name as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Resource as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Type as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::SpanId as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::ParentId as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Start as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Duration as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Error as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Env as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Version as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Component as u8 }>: TraceValueOp<D>,
    SpanValue<Self, D, { SpanValueType::Kind as u8 }>: TraceValueOp<D>,
    // TODO and mut variants
{
    // Safety note: Not only may it not be transferred across threads, but it also must not do callbacks (to avoid two mut references)
    type Storage: ?Send;
    type TraceRef;
    type ChunkRef;
    type SpanRef;
    type SpanLinkRef;
    type SpanEventRef;
    type AttributeRef;

    fn project(&mut self) -> Traces<Self::Storage, D>;

    fn chunk_iterator(trace: &Self::TraceRef) -> std::slice::Iter<Self::ChunkRef>;
    fn span_iterator(chunk: &Self::ChunkRef) -> std::slice::Iter<Self::SpanRef>;
    fn span_link_iterator(span: &Self::SpanRef) -> std::slice::Iter<Self::SpanLinkRef>;
    fn span_events_iterator(span: &Self::SpanRef) -> std::slice::Iter<Self::SpanEventRef>;
}

const IMMUT: u8 = 0;
pub const MUT: u8 = 1;

unsafe fn as_mut<T>(v: &T) -> &mut T {
    &mut *(v as *const _ as *mut _)
}

pub struct TraceValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    traces: &'a Traces<'a, T, D, Mut>,
}

pub struct ChunkValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    chunk: &'a T::ChunkRef,
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8> ChunkValue<'a, T, D, Type, MUT> {
    pub fn storage(&self) -> &'a T::Storage {
        self.storage
    }

    pub fn chunk(&self) -> &'a T::ChunkRef {
        self.storage
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Type: u8> ChunkValue<'a, T, D, Type, MUT> {
    pub fn as_mut(&mut self) -> (&'a mut T::Storage, &'a mut T::ChunkRef) {
        // SATEFY: As given by invariants on TraceProjector::Storage / this being MUT
        (
            &mut unsafe { *(self.storage as *const _ as *mut _) },
            &mut unsafe { *(self.chunk as *const _ as *mut _) },
        )
    }
}

pub struct SpanValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    span: &'a T::SpanRef,
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Type: u8> SpanValue<'a, T, D, Type, MUT> {
    pub fn mut_storage(&mut self) -> &'a mut T::Storage {
        // SATEFY: As given by invariants on TraceProjector::Storage
        &mut unsafe { *(self.storage as *const _ as *mut _) }
    }

    pub fn mut_span(&mut self) -> &'a mut T::SpanRef {
        // SATEFY: Exclusive by this being MUT
        &mut unsafe { *(self.span as *const _ as *mut _) }
    }
}

pub struct SpanLinkValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    link: &'a T::SpanLinkRef,
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Type: u8> SpanLinkValue<'a, T, D, Type, MUT> {
    pub fn mut_storage(&mut self) -> &'a mut T::Storage {
        // SATEFY: As given by invariants on TraceProjector::Storage
        &mut unsafe { *(self.storage as *const _ as *mut _) }
    }

    pub fn mut_span(&mut self) -> &'a mut T::SpanLinkRef {
        // SATEFY: Exclusive by this being MUT
        &mut unsafe { *(self.link as *const _ as *mut _) }
    }
}

pub struct SpanEventValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    event: &'a T::SpanEventRef,
}

impl<'a, T: TraceProjector<D>, D: TraceData, const Type: u8> SpanEventValue<'a, T, D, Type, MUT> {
    pub fn mut_storage(&mut self) -> &'a mut T::Storage {
        // SATEFY: As given by invariants on TraceProjector::Storage
        &mut unsafe { *(self.storage as *const _ as *mut _) }
    }

    pub fn mut_span(&mut self) -> &'a mut T::SpanRef {
        // SATEFY: Exclusive by this being MUT
        &mut unsafe { *(self.event as *const _ as *mut _) }
    }
}

pub trait TraceValueDataType<D: TraceData> {
    type Value;
}

pub trait TraceValueOp<S, C, D: TraceData>: TraceValueDataType<D> {
    fn get(storage: &S, chunk: &C) -> Self::Value;
}

pub trait TraceValueMutOp<S, C, D: TraceData>: TraceValueOp<S, C, D>  {
    fn set(storage: &mut S, chunk: &mut C, value: <Self as TraceValueDataType<D>>::Value);
}

impl<V: TraceValueOp<T, T::Storage, T::SpanRef>, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8> SpanValue<T, D, Type, Mut> {
    fn get_self(&self) {
        TraceValueOp::get(&self.storage, &self.span)
    }
}

impl<V: TraceValueMutOp<T, T::Storage, T::SpanRef>, T: TraceProjector<D>, D: TraceData, const Type: u8> SpanValue<T, D, Type, MUT> {
    fn set_self(&self, value: <Self as TraceValueDataType<D>>::Value) {
        unsafe { TraceValueMutOp::set(as_mut(self.storage), as_mut(self.span), value) }
    }
}

pub enum TraceValueType {
    ContainerId = 1,
    LanguageName = 2,
    LanguageVersion = 3,
    TracerVersion = 4,
    RuntimeId = 5,
    Env = 6,
    Hostname = 7,
    AppVersion = 8,
}

impl<T, D: TraceData, const Type: u8> TraceValueDataType<D> for TraceValue<T, D, Type> {
    type Value = String;
}

#[derive(Debug, Copy, Clone)]
pub struct Traces<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage, // pin?
}
pub type TracesMut<'a, T, D> = Traces<'a, T, D, MUT>;

impl<T: TraceProjector<D>, D: TraceData, const Mut: u8> Traces<T, D, Mut> {
    fn value<const Type: u8>(&self) -> TraceValue<T, D, Type> {
        TraceValue {
            traces: self,
        }
    }

    pub fn container_id(&self) -> D::Text {
        self.value::<{ TraceValueType::ContainerId as u8 }>().get()
    }

    pub fn language_name(&self) -> D::Text {
        self.value::<{ TraceValueType::LanguageName as u8 }>().get()
    }

    pub fn language_version(&self) -> D::Text {
        self.value::<{ TraceValueType::LanguageVersion as u8 }>().get()
    }

    pub fn tracer_version(&self) -> D::Text {
        self.value::<{ TraceValueType::TracerVersion as u8 }>().get()
    }

    pub fn runtime_id(&self) -> D::Text {
        self.value::<{ TraceValueType::RuntimeId as u8 }>().get()
    }

    pub fn env(&self) -> D::Text {
        self.value::<{ TraceValueType::Env as u8 }>().get()
    }

    pub fn hostname(&self) -> D::Text {
        self.value::<{ TraceValueType::Hostname as u8 }>().get()
    }

    pub fn app_version(&self) -> D::Text {
        self.value::<{ TraceValueType::AppVersion as u8 }>().get()
    }

    pub fn attributes(&self) -> TraceAttributes<T, D, T::TraceRef, Mut> {
        TraceAttributes {
            storage: self.storage,
            container: self,
        }
    }

    pub fn chunks(&self) -> Nested<T, D, T::TraceRef, T::ChunkRef, Mut> {
        Nested {
            storage: self.storage,
            container: self,
            _phantom: PhantomData,
        }
    }
}

impl <T: TraceProjector<D>, D: TraceData> TracesMut<'_, T, D> {
    pub fn set_container_id<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::ContainerId as u8 }>().set(value.into())
    }

    pub fn set_language_name<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::LanguageName as u8 }>().set(value.into())
    }

    pub fn set_language_version<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::LanguageVersion as u8 }>().set(value.into())
    }

    pub fn set_tracer_version<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::TracerVersion as u8 }>().set(value.into())
    }

    pub fn set_runtime_id<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::RuntimeId as u8 }>().set(value.into())
    }

    pub fn set_env<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::Env as u8 }>().set(value.into())
    }

    pub fn set_hostname<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::Hostname as u8 }>().set(value.into())
    }

    pub fn set_app_version<I: Into<D::Text>>(&mut self, value: I) {
        self.value::<{ TraceValueType::AppVersion as u8 }>().set(value.into())
    }
}

pub struct Nested<'a, T: TraceProjector<D>, D: TraceData, C, I, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    container: &'a C,
    _phantom: PhantomData<I>,
}

struct NestedIterator<'a, T: TraceProjector<D>, D: TraceData, I: Iterator> {
    storage: &'a T::Storage,
    it: I,
}

impl<T: TraceProjector<D>, D: TraceData, I: Iterator<Item = T::ChunkRef>> Iterator for NestedIterator<T, D, I> {
    type Item = TraceChunk<T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|chunk| {
            TraceChunk {
                storage: self.storage,
                chunk,
            }
        })
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> IntoIterator for &'a mut Nested<'a, T, D, T::TraceRef, T::ChunkRef> {
    type Item = TraceChunk<T, D>;
    type IntoIter = NestedIterator<'a, T, D, std::slice::Iter<'a, T::ChunkRef>>;

    fn into_iter(self) -> Self::IntoIter {
        NestedIterator {
            storage: self.storage,
            it: T::chunk_iterator(self.container)
        }
    }
}

pub enum ChunkValueType {
    Priority = 1,
    Origin = 2,
    DroppedTrace = 3,
    TraceId = 4,
    SamplingMechanism = 5,
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for ChunkValue<T, D, { ChunkValueType::Priority as u8 }, Mut> {
    type Value = i32;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for ChunkValue<T, D, { ChunkValueType::Origin as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for ChunkValue<T, D, { ChunkValueType::DroppedTrace as u8 }, Mut> {
    type Value = bool;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for ChunkValue<T, D, { ChunkValueType::TraceId as u8 }, Mut> {
    type Value = u128;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for ChunkValue<T, D, { ChunkValueType::SamplingMechanism as u8 }, Mut> {
    type Value = u32;
}

#[derive(Debug, Copy, Clone)]
pub struct TraceChunk<T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: T::Storage,
    chunk: T::ChunkRef,
}

impl<T: TraceProjector<D>, D: TraceData, const Mut: u8> TraceChunk<T, D, Mut> {
    fn value<const Type: u8>(&self) -> ChunkValue<T, D, Type, Mut> {
        ChunkValue {
            chunk: self.chunk,
            storage: self.storage,
        }
    }

    pub fn priority(&self) -> ChunkValue<T, D, { ChunkValueType::Priority as u8 }, Mut> {
        self.value()
    }

    pub fn origin(&self) -> ChunkValue<T, D, { ChunkValueType::Origin as u8 }, Mut> {
        self.value()
    }

    pub fn dropped_trace(&self) -> ChunkValue<T, D, { ChunkValueType::DroppedTrace as u8 }, Mut> {
        self.value()
    }

    pub fn trace_id(&self) -> ChunkValue<T, D, { ChunkValueType::TraceId as u8 }, Mut> {
        self.value()
    }

    pub fn sampling_mechanism(&self) -> ChunkValue<T, D, { ChunkValueType::SamplingMechanism as u8 }, Mut> {
        self.value()
    }

    pub fn attributes(&self) -> TraceAttributes<T, D, T::ChunkRef, Mut> {
        TraceAttributes {
            storage: self.storage,
            container: self,
        }
    }

    pub fn spans(&self) -> Nested<T, D, T::ChunkRef, T::SpanRef, Mut> {
        Nested {
            storage: self.storage,
            container: self,
            _phantom: PhantomData,
        }
    }
}


impl<T: TraceProjector<D>, D: TraceData, I: Iterator<Item = T::SpanRef>> Iterator for NestedIterator<T, D, I> {
    type Item = TraceChunk<T, D>;

    fn next(&mut self) -> Option<Self::Item> {
        self.it.next().map(|chunk| {
            TraceChunk {
                storage: self.storage,
                chunk,
            }
        })
    }
}

impl<'a, T: TraceProjector<D>, D: TraceData> IntoIterator for &'a mut Nested<T, D, T::ChunkRef, T::SpanRef> {
    type Item = TraceChunk<T, D>;
    type IntoIter = NestedIterator<'a, T, D, std::slice::Iter<'a, T::SpanRef>>;

    fn into_iter(self) -> Self::IntoIter {
        NestedIterator {
            storage: self.storage,
            it: T::span_iterator(self.container)
        }
    }
}

pub enum SpanValueType {
    Service = 1,
    Name = 2,
    Resource = 3,
    Type = 4,
    SpanId = 5,
    ParentId = 6,
    Start = 7,
    Duration = 8,
    Error = 9,
    Env = 10,
    Version = 11,
    Component = 12,
    Kind = 13,
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Service as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Name as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Resource as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Type as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::SpanId as u8 }, Mut> {
    type Value = u64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::ParentId as u8 }, Mut> {
    type Value = u64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Start as u8 }, Mut> {
    type Value = i64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Duration as u8 }, Mut> {
    type Value = i64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Error as u8 }, Mut> {
    type Value = bool;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Env as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Version as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Component as u8 }, Mut> {
    type Value = String;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanValue<T, D, { SpanValueType::Kind as u8 }, Mut> {
    type Value = SpanKind;
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
#[derive(Debug, Copy, Clone)]
pub struct Span<T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: T::Storage,
    span: T::SpanRef,
}

impl<T: TraceProjector<D>, D: TraceData, const Mut: u8> Span<T, D, Mut> {
    fn value<const Type: u8>(&self) -> SpanValue<T, D, Type, Mut> {
        SpanValue {
            span: self.span,
            storage: self.storage,
        }
    }

    pub fn service(&self) -> SpanValue<T, D, { SpanValueType::Service as u8 }, Mut> {
        self.value()
    }

    pub fn name(&self) -> SpanValue<T, D, { SpanValueType::Name as u8 }, Mut> {
        self.value()
    }

    pub fn resource(&self) -> SpanValue<T, D, { SpanValueType::Resource as u8 }, Mut> {
        self.value()
    }

    pub fn r#type(&self) -> SpanValue<T, D, { SpanValueType::Type as u8 }, Mut> {
        self.value()
    }

    pub fn span_id(&self) -> SpanValue<T, D, { SpanValueType::SpanId as u8 }, Mut> {
        self.value()
    }

    pub fn parent_id(&self) -> SpanValue<T, D, { SpanValueType::ParentId as u8 }, Mut> {
        self.value()
    }

    pub fn start(&self) -> SpanValue<T, D, { SpanValueType::Start as u8 }, Mut> {
        self.value()
    }

    pub fn duration(&self) -> SpanValue<T, D, { SpanValueType::Duration as u8 }, Mut> {
        self.value()
    }

    pub fn error(&self) -> SpanValue<T, D, { SpanValueType::Error as u8 }, Mut> {
        self.value()
    }

    pub fn env(&self) -> SpanValue<T, D, { SpanValueType::Env as u8 }, Mut> {
        self.value()
    }

    pub fn version(&self) -> SpanValue<T, D, { SpanValueType::Version as u8 }, Mut> {
        self.value()
    }

    pub fn component(&self) -> SpanValue<T, D, { SpanValueType::Component as u8 }, Mut> {
        self.value()
    }

    pub fn kind(&self) -> SpanValue<T, D, { SpanValueType::Kind as u8 }, Mut> {
        self.value()
    }


    pub fn attributes(&self) -> TraceAttributes<T, D, T::SpanRef, Mut> {
        TraceAttributes {
            storage: self.storage,
            container: self,
        }
    }

    pub fn span_links(&self) -> Nested<T, D, T::SpanRef, T::SpanLinkRef, Mut> {
        Nested {
            storage: self.storage,
            container: self,
            _phantom: PhantomData,
        }
    }

    pub fn span_events(&self) -> Nested<T, D, T::SpanRef, T::SpanEventRef, Mut> {
        Nested {
            storage: self.storage,
            container: self,
            _phantom: PhantomData,
        }
    }
}

pub enum SpanLinkType {
    TraceId = 1,
    SpanId = 2,
    TraceState = 3,
    Flags = 4,
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanLinkValue<T, D, { SpanLinkType::TraceId as u8 }, Mut> {
    type Value = u128;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanLinkValue<T, D, { SpanLinkType::SpanId as u8 }, Mut> {
    type Value = u64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanLinkValue<T, D, { SpanLinkType::TraceState as u8 }, Mut> {
    type Value = D::Text;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for SpanLinkValue<T, D, { SpanLinkType::Flags as u8 }, Mut> {
    type Value = u32;
}


/// The generic representation of a V04 span link.
/// `T` is the type used to represent strings in the span link.
#[derive(Debug)]
pub struct SpanLink<T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: T::Storage,
    link: T::SpanLinkRef,
}

impl<T: TraceProjector<D>, D: TraceData, const Mut: u8> SpanLink<T, D, Mut> {
    fn value<const Type: u8>(&self) -> SpanLinkValue<T, D, Type, Mut> {
        SpanLinkValue {
            link: self.link,
            storage: self.storage,
        }
    }

    pub fn trace_id(&self) -> SpanLinkValue<T, D, { SpanLinkType::TraceId as u8 }, Mut> {
        self.value()
    }

    pub fn span_id(&self) -> SpanLinkValue<T, D, { SpanLinkType::SpanId as u8 }, Mut> {
        self.value()
    }

    pub fn trace_state(&self) -> SpanLinkValue<T, D, { SpanLinkType::TraceState as u8 }, Mut> {
        self.value()
    }

    pub fn flags(&self) -> SpanLinkValue<T, D, { SpanLinkType::Flags as u8 }, Mut> {
        self.value()
    }

    pub fn attributes(&self) -> TraceAttributes<T, D, T::SpanLinkRef, Mut> {
        TraceAttributes {
            storage: self.storage,
            container: self,
        }
    }
}

pub enum SpanEventType {
    TimeUnixNano = 1,
    Name = 2,
}

impl<T, D: TraceData> TraceValueDataType<D> for SpanEventValue<T, D, { SpanEventType::TimeUnixNano as u8 }> {
    type Value = u64;
}

impl<T, D: TraceData> TraceValueDataType<D> for SpanEventValue<T, D, { SpanEventType::Name as u8 }> {
    type Value = D::Text;
}


/// The generic representation of a V04 span event.
/// `T` is the type used to represent strings in the span event.
#[derive(Debug)]
pub struct SpanEvent<T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: T::Storage,
    event: T::SpanEventRef,
}

impl<T: TraceProjector<D>, D: TraceData, const Mut: u8> SpanEvent<T, D, Mut> {
    fn value<const Type: u8>(&self) -> SpanEventValue<T, D, Type, Mut> {
        SpanEventValue {
            storage: self.storage,
            event: self.event,
        }
    }

    pub fn time_unix_nano(&self) -> SpanEventValue<T, D, { SpanEventType::TimeUnixNano as u8 }, Mut> {
        self.value()
    }

    pub fn name(&self) -> SpanEventValue<T, D, { SpanEventType::Name as u8 }, Mut> {
        self.value()
    }
}

enum AttributeAnyValueType {
    String = 1,
    Bytes = 2,
    Boolean = 3,
    Integer = 4,
    Double = 5,
    Array = 6,
    Map = 7,
}

pub struct AttributeInnerValue<'a, T: TraceProjector<D>, D: TraceData, const Type: u8, const Mut: u8 = IMMUT> {
    pub storage: &'a mut T::Storage,
    pub container: &'a mut T::AttributeRef,
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for AttributeInnerValue<T, D, { AttributeAnyValueType::String as u8 }, Mut> {
    type Value = D::Text;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for AttributeInnerValue<T, D, { AttributeAnyValueType::Bytes as u8 }, Mut> {
    type Value = D::Bytes;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for AttributeInnerValue<T, D, { AttributeAnyValueType::Boolean as u8 }, Mut> {
    type Value = bool;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for AttributeInnerValue<T, D, { AttributeAnyValueType::Integer as u8 }, Mut> {
    type Value = i64;
}

impl<T, D: TraceData, const Mut: u8> TraceValueDataType<D> for AttributeInnerValue<T, D, { AttributeAnyValueType::Double as u8 }, Mut> {
    type Value = f64;
}

pub struct AttributeArray<'a, T: TraceProjector<D>, D: TraceData, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    container: &'a T::AttributeRef,
}

pub trait AttributeArrayOp<T: TraceProjector<D>, D: TraceData>: Iterator<Item = AttributeAnyValue<T, D>> {
    fn append(&self) -> AttributeAnyValue<T, D>;
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> AttributeAnyValue<T, D>;
}

#[derive(Debug, PartialEq)]
pub enum AttributeAnyValue<'a, T: TraceProjector<D>, D: TraceData> {
    String(AttributeInnerValue<'a, T, D, { AttributeAnyValueType::String as u8 }>),
    Bytes(AttributeInnerValue<'a, T, D, { AttributeAnyValueType::Bytes as u8 }>),
    Boolean(AttributeInnerValue<'a, T, D, { AttributeAnyValueType::Boolean as u8 }>),
    Integer(AttributeInnerValue<'a, T, D, { AttributeAnyValueType::Integer as u8 }>),
    Double(AttributeInnerValue<'a, T, D, { AttributeAnyValueType::Double as u8 }>),
    Array(AttributeArray<'a, T, D>),
    Map(TraceAttributes<'a, T, D, T::AttributeRef>)
}

pub struct TraceAttributes<'a, T: TraceProjector<D>, D: TraceData, C, const Mut: u8 = IMMUT> {
    storage: &'a T::Storage,
    container: &'a C,
}

trait TraceAttributesOp<T: TraceProjector<D>, D: TraceData>
{
    fn set(&mut self, key: &str, value: AttributeAnyValueType) -> AttributeAnyValue<T, D>;
    fn get(&self, key: &str) -> Option<AttributeAnyValue<T, D>>;
    fn remove(&mut self, key: &str);
}

impl<T: TraceProjector<D>, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::String as u8 }>: TraceValueMutOp<D, T::Storage, T::AttributeRef>
{
    fn set_string(&mut self, key: &str, value: D::Text) {
        let AttributeAnyValue::String(inner) = self.set(key, AttributeAnyValueType::String) else { unreachable!() };
        inner.set(value);
    }
}

impl<T: TraceProjector<D>, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::String as u8 }>: TraceValueOp<D, T::Storage, T::AttributeRef>
{
    fn get_string(&self, key: &str) -> Option<D::Text> {
        if let Some(AttributeAnyValue::String(inner)) = self.get(key) {
            Some(inner.get())
        } else {
            None
        }
    }
}

impl<T, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::Bytes as u8 }>: TraceValueOp<D> {
    fn set_bytes(&mut self, key: &str, value: D::Bytes) {
        let AttributeAnyValue::Bytes(inner) = self.set(key, AttributeAnyValueType::Bytes) else { unreachable!() };
        inner.set(value);
    }

    fn get_bytes(&self, key: &str) -> Option<D::Bytes> {
        if let Some(AttributeAnyValue::Bytes(inner)) = self.get(key) {
            Some(inner.get())
        } else {
            None
        }
    }
}

impl<T, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::Boolean as u8 }>: TraceValueOp<D> {

    fn set_bool(&mut self, key: &str, value: bool) {
        let AttributeAnyValue::Boolean(inner) = self.set(key, AttributeAnyValueType::Boolean) else { unreachable!() };
        inner.set(value);
    }

    fn get_bool(&self, key: &str) -> Option<bool> {
        if let Some(AttributeAnyValue::Boolean(inner)) = self.get(key) {
            Some(inner.get())
        } else {
            None
        }
    }
}

impl<T, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::Integer as u8 }>: TraceValueOp<D> {

    fn set_int(&mut self, key: &str, value: i64) {
        let AttributeAnyValue::Integer(inner) = self.set(key, AttributeAnyValueType::Integer) else { unreachable!() };
        inner.set(value);
    }

    fn get_int(&self, key: &str) -> Option<i64> {
        if let Some(AttributeAnyValue::Integer(inner)) = self.get(key) {
            Some(inner.get())
        } else {
            None
        }
    }
}

impl<T, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D>, AttributeInnerValue<T, D, { AttributeAnyValueType::Double as u8 }>: TraceValueOp<D> {

    fn set_double(&mut self, key: &str, value: f64) {
        let AttributeAnyValue::Double(inner) = self.set(key, AttributeAnyValueType::Double) else { unreachable!() };
        inner.set(value);
    }

    fn get_double(&self, key: &str) -> Option<f64> {
        if let Some(AttributeAnyValue::Double(inner)) = self.get(key) {
            Some(inner.get())
        } else {
            None
        }
    }
}

impl<T: TraceProjector<D>, D: TraceData, C> TraceAttributes<T, D, C> where Self: TraceAttributesOp<T, D> {

    fn set_array(&mut self, key: &str) -> AttributeArray<T, D> {
        let AttributeAnyValue::Array(inner) = self.set(key, AttributeAnyValueType::Array) else { unreachable!() };
        inner
    }

    fn get_array(&self, key: &str) -> Option<AttributeArray<T, D>> {
        if let Some(AttributeAnyValue::Array(inner)) = self.get(key) {
            Some(inner)
        } else {
            None
        }
    }

    fn set_map(&mut self, key: &str) -> TraceAttributes<T, D, T::AttributeRef> {
        let AttributeAnyValue::Map(inner) = self.set(key, AttributeAnyValueType::Map) else { unreachable!() };
        inner
    }

    fn get_map(&self, key: &str) -> Option<TraceAttributes<T, D, T::AttributeRef>> {
        if let Some(AttributeAnyValue::Map(inner)) = self.get(key) {
            Some(inner)
        } else {
            None
        }
    }
}
