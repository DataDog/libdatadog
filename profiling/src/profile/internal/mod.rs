// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

mod function;
mod label;
mod line;
mod location;
mod mapping;
mod stack_trace;
mod string;
mod value_type;

pub use function::*;
pub use label::*;
pub use line::*;
pub use location::*;
pub use mapping::*;
pub use stack_trace::*;
pub use string::*;
pub use value_type::*;

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

    fn to_pprof(&self, id: Self::Id) -> Self::PprofMessage;
}

#[inline]
fn small_non_zero_pprof_id(offset: usize) -> Option<NonZeroU32> {
    let small: u32 = offset.try_into().ok()?;
    let non_zero = small.checked_add(1)?;
    // Safety: the `checked_add(1)?` guards this from ever being zero.
    Some(unsafe { NonZeroU32::new_unchecked(non_zero) })
}
