// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod error;
mod set;
mod slice_set;
mod string_set;
mod thin_str;

pub type SetHasher = core::hash::BuildHasherDefault<rustc_hash::FxHasher>;

pub use error::*;
pub use set::*;
pub use slice_set::*;
pub use string_set::*;
pub use thin_str::*;

use std::any::TypeId;

/// Operations that somewhat abstract around Sets of single items and sets of
/// slices for sharded sets.
///
/// # Safety
///
/// Implementors must ensure that all methods which take `&self` are safe to
/// call under a read-lock, and all `&mut self` methods are safe to call under
/// a write-lock, and are safe for `Send` and `Sync`.
pub unsafe trait ShardedSetOps {
    type Lookup<'a>: Copy
    where
        Self: 'a;

    /// Owned payload used for insertion. For some containers (e.g. slice-backed
    /// sets) this can be a borrowed view like `&'a [T]` because the container
    /// copies data into its own arena during insertion.
    type Owned<'a>
    where
        Self: 'a;

    type Id: Copy;

    /// Returns the `TypeId` of the logical element type stored by this set.
    fn type_id(&self) -> TypeId;

    fn try_with_capacity(capacity: usize) -> Result<Self, SetError>
    where
        Self: Sized;

    fn len(&self) -> usize;

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// # Safety
    /// Same safety contract as the underlying container's find_with_hash.
    unsafe fn find_with_hash(&self, hash: u64, key: Self::Lookup<'_>) -> Option<Self::Id>;

    /// # Safety
    /// Same safety contract as the underlying container's insert_unique_uncontended_with_hash.
    unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        key: Self::Owned<'_>,
    ) -> Result<Self::Id, SetError>;
}
