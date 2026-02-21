const IMMUT: u8 = 0;
const MUT: u8 = 1;

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
