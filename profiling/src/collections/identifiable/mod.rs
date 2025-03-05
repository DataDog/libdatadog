// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod string_id;

use anyhow::Context;
use std::hash::{BuildHasherDefault, Hash};
use std::num::NonZeroU32;

pub type FxIndexMap<K, V> = indexmap::IndexMap<K, V, BuildHasherDefault<rustc_hash::FxHasher>>;
pub type FxIndexSet<K> = indexmap::IndexSet<K, BuildHasherDefault<rustc_hash::FxHasher>>;

pub use string_id::*;

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
pub fn small_non_zero_pprof_id(offset: usize) -> Option<NonZeroU32> {
    let small: u32 = offset.try_into().ok()?;
    let non_zero = small.checked_add(1)?;
    // Safety: the `checked_add(1)?` guards this from ever being zero.
    Some(unsafe { NonZeroU32::new_unchecked(non_zero) })
}

pub trait Dedup<T: Item> {
    /// Deduplicate the Item and return its associated Id.
    /// # Panics
    /// Panics if the number of items overflows the storage capabilities of
    /// the associated Id type.
    fn dedup(&mut self, item: T) -> <T as Item>::Id;

    /// Deduplicate the Item, and check if the generated Id is valid.
    fn checked_dedup(&mut self, item: T) -> anyhow::Result<<T as Item>::Id>;
}

impl<T: Item> Dedup<T> for FxIndexSet<T> {
    fn dedup(&mut self, item: T) -> <T as Item>::Id {
        let (id, _) = self.insert_full(item);
        <T as Item>::Id::from_offset(id)
    }

    /// In incident 35390 (JIRA PROF-11456) we observed invalid location_ids being present in
    /// emitted profiles. It's not likely that the incorrect ids are coming from the underlying
    /// collection, but we're doing extra checks here so that if we see incorrect ids again,
    /// we are 100% sure they were not introduced at this stage.
    fn checked_dedup(&mut self, item: T) -> anyhow::Result<<T as Item>::Id> {
        let (id, _) = self.insert_full(item);

        anyhow::ensure!(
            id < self.len(),
            "out of bounds id generated {:?}, len was {:?}",
            id,
            self.len()
        );
        small_non_zero_pprof_id(id).with_context(|| format!("invalid id generated {:?}", id))?;

        Ok(<T as Item>::Id::from_offset(id))
    }
}

pub fn into_pprof_iter<T: PprofItem>(
    collection: FxIndexSet<T>,
) -> impl Iterator<Item = T::PprofMessage> {
    collection
        .into_iter()
        .enumerate()
        .map(|(index, item)| item.to_pprof(<T as Item>::Id::from_offset(index)))
}

#[cfg(test)]
mod tests {
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
