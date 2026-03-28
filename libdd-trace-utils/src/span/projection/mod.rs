//! Abstract projection layer for reading and writing trace data through a generic [`TraceProjector`].
//!
//! The projection API decouples trace data access from its underlying storage representation.
//! A [`TraceProjector`] implementation defines how fields are mapped to storage; the view
//! types ([`Traces`], [`TraceChunk`], [`Span`], [`SpanLink`], [`SpanEvent`]) expose an
//! ergonomic read/write API on top of those mappings.
//!
//! Attribute maps are accessed via [`TraceAttributes`] and attribute arrays via [`AttributeArray`].
//!
//! # Mutability
//!
//! Immutable view types (e.g. [`Traces`]) and their mutable aliases (e.g. [`TracesMut`]) are the
//! same struct parameterised by a `const ISMUT` flag. Mutable variants additionally expose setter
//! methods and are obtained through `project_mut()` on the projector.

const IMMUT: u8 = 0;
const MUT: u8 = 1;

// Cast a shared reference to a mutable one. Only sound when the caller guarantees
// exclusive access through the broader borrow structure (e.g. the storage and container
// are borrowed from the same struct that is exclusively borrowed at a higher level).
#[allow(invalid_reference_casting)]
unsafe fn as_mut<T>(v: &T) -> &mut T {
    &mut *(v as *const _ as *mut _)
}

mod projector;
pub use projector::*;

mod traces;
pub use traces::*;

mod trace_chunk;
pub use trace_chunk::*;

mod span;
pub use span::*;

mod span_link;
pub use span_link::*;

mod span_event;
pub use span_event::*;

mod attributes;
pub use attributes::*;

mod attribute_array;
pub use attribute_array::*;
