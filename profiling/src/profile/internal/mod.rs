// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod endpoints;
mod function;
mod label;
mod location;
mod mapping;
mod sample;
mod stack_trace;
mod string;
mod upscaling;
mod value_type;

pub use endpoints::*;
pub use function::*;
pub use label::*;
pub use location::*;
pub use mapping::*;
pub use sample::*;
pub use stack_trace::*;
pub use string::*;
pub use upscaling::*;
pub use value_type::*;

use super::pprof;
use std::hash::Hash;
use std::num::NonZeroU32;

pub trait Id: Copy + Eq + Hash {
    type RawId;

    /// Convert from a usize offset into an Id. This should be loss-less
    /// except for certain edges.
    /// # Panics
    /// Panic if the usize cannot be represented in the Id, for instance if
    /// the offset cannot fit in the underlying integer type. This is expected
    /// to be ultra-rare (more than u32::MAX-1 items created?!).
    fn from_offset(inner: usize) -> Self;

    fn to_raw_id(&self) -> Self::RawId;

    fn into_raw_id(self) -> Self::RawId {
        self.to_raw_id()
    }
}

pub trait Item: Eq + Hash {
    /// The Id associated with this Item, e.g. Function -> FunctionId.
    type Id: Id;
}

/// Used to associate an Item with a pprof::* type. Not all Items can be
/// converted to pprof::* types. For example, StackTrace doesn't have an
/// associated pprof::* type.
pub trait PprofItem: Item {
    /// The pprof::* type associated with this Item.
    /// For example, Function -> pprof::Function.
    type PprofMessage: prost::Message;

    // This function exists because items don't store their own id, so the
    // items can't do a simple .into() to get a pprof message.
    fn to_pprof(&self, id: Self::Id) -> Self::PprofMessage;
}

/// Creates a non-zero, 32-bit unsigned id from the offset. It's guaranteed to
/// be the offset + 1, with guards to not overflow the size of u32.
///
/// This is useful because many pprof collections do not allow an item with an
/// id of zero, even if it's the first item in the collection.
#[inline]
fn small_non_zero_pprof_id(offset: usize) -> Option<NonZeroU32> {
    let small: u32 = offset.try_into().ok()?;
    let non_zero = small.checked_add(1)?;
    // Safety: the `checked_add(1)?` guards this from ever being zero.
    Some(unsafe { NonZeroU32::new_unchecked(non_zero) })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_small_non_zero_pprof_id() {
        assert_eq!(NonZeroU32::new(1), small_non_zero_pprof_id(0));
        assert_eq!(NonZeroU32::new(2), small_non_zero_pprof_id(1));
        assert_eq!(
            NonZeroU32::new(u32::MAX),
            small_non_zero_pprof_id((u32::MAX - 1) as usize)
        );

        assert_eq!(None, small_non_zero_pprof_id(u32::MAX as usize));
        assert_eq!(None, small_non_zero_pprof_id(usize::MAX));
    }
}
