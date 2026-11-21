// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! An insertion-ordered hash set: an [`IndexMap`] without values.
//!
//! Each key has a stable index, `0..len` in the order keys were first
//! inserted, which makes the set an interner: the index is the key's id, and
//! [`IndexSet::get_index`] resolves an id back to its key. Keys are never
//! removed, and every allocation is fallible.

use crate::collections::alloc::{AllocError, Allocator};
use crate::collections::index_map::{self, DefaultHashBuilder, Equivalent, IndexMap};
use core::fmt;
use core::hash::{BuildHasher, Hash};
use core::iter::FusedIterator;

/// An insertion-ordered hash set with a caller-chosen [`Allocator`].
pub struct IndexSet<K, A: Allocator, S = DefaultHashBuilder> {
    map: IndexMap<K, (), A, S>,
}

impl<K, A: Allocator, S: Default> IndexSet<K, A, S> {
    /// An empty set that allocates nothing until the first insertion.
    #[must_use]
    pub fn new_in(alloc: A) -> Self {
        Self {
            map: IndexMap::new_in(alloc),
        }
    }

    /// An empty set with room for at least `capacity` keys.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated, or if `capacity`
    /// exceeds `u32::MAX`.
    pub fn try_with_capacity_in(capacity: usize, alloc: A) -> Result<Self, AllocError> {
        Ok(Self {
            map: IndexMap::try_with_capacity_in(capacity, alloc)?,
        })
    }
}

impl<K, A: Allocator, S> IndexSet<K, A, S> {
    /// An empty set that uses `hash_builder` to hash keys.
    #[must_use]
    pub const fn with_hasher_in(hash_builder: S, alloc: A) -> Self {
        Self {
            map: IndexMap::with_hasher_in(hash_builder, alloc),
        }
    }

    /// The number of keys.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// How many keys fit before the set allocates again.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.map.capacity()
    }

    /// The key at `index`, its insertion position.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<&K> {
        self.map.get_index(index).map(|(key, ())| key)
    }

    /// The keys, in insertion order.
    pub fn iter(&self) -> Iter<'_, K> {
        Iter(self.map.iter())
    }

    /// Makes room for at least `additional` more keys.
    ///
    /// # Errors
    ///
    /// [`AllocError`]; the set is unchanged.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), AllocError> {
        self.map.try_reserve(additional)
    }
}

impl<K, A: Allocator, S: BuildHasher> IndexSet<K, A, S> {
    /// The index of `key`, if present.
    #[inline]
    pub fn get_index_of<Q>(&self, key: &Q) -> Option<usize>
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.map.get_index_of(key)
    }

    #[inline]
    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        Q: Hash + Equivalent<K> + ?Sized,
    {
        self.map.contains_key(key)
    }

    /// Adds `key` if absent. Returns its index and whether it was added; an
    /// equal key already present keeps its index and `key` is dropped.
    ///
    /// # Errors
    ///
    /// [`AllocError`]; the set is unchanged.
    pub fn try_insert(&mut self, key: K) -> Result<(usize, bool), AllocError>
    where
        K: Hash + Eq,
    {
        let (index, old) = self.map.try_insert(key, ())?;
        Ok((index, old.is_none()))
    }
}

impl<K, A: Allocator + Default, S: Default> Default for IndexSet<K, A, S> {
    fn default() -> Self {
        Self::new_in(A::default())
    }
}

impl<K: fmt::Debug, A: Allocator, S> fmt::Debug for IndexSet<K, A, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl<'s, K, A: Allocator, S> IntoIterator for &'s IndexSet<K, A, S> {
    type Item = &'s K;
    type IntoIter = Iter<'s, K>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The keys of an [`IndexSet`], in insertion order.
pub struct Iter<'s, K>(index_map::Iter<'s, K, ()>);

impl<'s, K> Iterator for Iter<'s, K> {
    type Item = &'s K;

    fn next(&mut self) -> Option<&'s K> {
        self.0.next().map(|(key, ())| key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<K> DoubleEndedIterator for Iter<'_, K> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(key, ())| key)
    }
}

impl<K> ExactSizeIterator for Iter<'_, K> {}
impl<K> FusedIterator for Iter<'_, K> {}

impl<K: fmt::Debug> fmt::Debug for Iter<'_, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Iter").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::alloc::Global;

    #[test]
    fn indexes_follow_first_insertion() {
        let mut set = IndexSet::<&str, Global>::new_in(Global);
        assert_eq!(set.try_insert("a"), Ok((0, true)));
        assert_eq!(set.try_insert("b"), Ok((1, true)));
        assert_eq!(set.try_insert("a"), Ok((0, false)));
        assert_eq!(set.len(), 2);
        assert_eq!(set.get_index_of("b"), Some(1));
        assert_eq!(set.get_index(1), Some(&"b"));
        assert_eq!(set.get_index(2), None);
        assert!(set.contains("a") && !set.contains("c"));
        assert!(set.iter().copied().eq(["a", "b"]));
    }
}
