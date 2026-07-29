// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module defines a associative map datastructure for spans data (meta, metrics, etc.) backed
//! by a vector. Spans are mostly allocated and constructed, and more rarely read or mutated.
//! [VecMap] is thus optimized for insertion (which is just `Vec::push`), without any hashing
//! involved. Fetching and removing a value is, on the other hand, linear time in the size of the
//! map. However, since meta and metrics are expected to be typically small (20ish elements or
//! less), linear scan is usually still competitive with hashmap's `get`.

use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};

/// A Vec-backed map that provides HashMap-like lookup by key.
///
/// # Duplicates
///
/// Duplicates are tolerated: [VecMap::insert] always appends, and [VecMap::get]/[VecMap::get_mut]
/// return the *last* matching entry so that later writes shadow earlier ones. This optimizes for
/// fast insertion and construction (that might happen on the client's application hot path),
/// avoiding a linear scan on each insert, or a potential full re-hashing with a hashmap.
/// Additionally, while overriding a metric or a meta definitively happens, it's assumed to be rare
/// enough so such that the size penalty of duplication is expected to be reasonable.
///
/// **Important**: note that only [VecMap::get] and [VecMap::get_mut] are duplicate-aware, so to
/// speak. [VecMap::len], [VecMap::iter], and others just delegates to the underlying `Vec`, and
/// won't deduplicate.
///
/// Explicit deduplication is currently being done on-demand by [VecMap::dedup]. An internal flag is
/// used to avoid undue deduplication (see [VecMap::dedup]). `VecMap` is automatically deduped
/// before serialization.
///
/// In the future, we could trigger deduplication on other events, for example at insertion if the
/// size is bigger than a threshold (and we haven't deduped for `x` operations).
///
/// # Ordering
///
/// As this is a map, iteration order is not defined nor guaranteed. In practice, iteration follows
/// insertion order, but [Self::dedup] will reverse the underlying vector.
#[derive(Clone, Debug)]
pub struct VecMap<K, V> {
    data: Vec<(K, V)>,
    /// Deduped is a flag that is set after entry deduplication. It is dirtied (set to `false`)
    /// when any modification that could create duplicates is performed (`deduped == false`
    /// doesn't imply there are actual duplicates, just than there might be). This is useful to
    /// avoid performing deduplication several times in a row, for example in the export
    /// pipeline.
    deduped: bool,
}

impl<K, V> Default for VecMap<K, V> {
    fn default() -> Self {
        Self {
            data: Default::default(),
            deduped: false,
        }
    }
}

// Only enabled for tests: this allocates (builds two `HashMap`s), which would be surprising
// behind a plain `==` in production code. Production callers should use `slow_compare` instead,
// so the cost is visible at the call site.
#[cfg(any(test, feature = "test-utils"))]
impl<K: Eq + Hash, V: PartialEq> PartialEq for VecMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.slow_compare(other)
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl<K: Eq + Hash, V: Eq> Eq for VecMap<K, V> {}

impl<K, V> VecMap<K, V> {
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dirty the `dedup` flag after a mutation that could introduce duplicates.
    fn dirty(&mut self) {
        self.deduped = false;
    }

    #[must_use]
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        VecMap {
            data: Vec::with_capacity(capacity),
            deduped: false,
        }
    }

    #[inline]
    pub fn insert(&mut self, key: K, value: V) {
        self.data.push((key, value));
        self.dirty();
    }

    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: ?Sized + PartialEq,
    {
        self.data
            .iter()
            .rev()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }

    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: ?Sized + PartialEq,
    {
        self.data
            .iter_mut()
            .rev()
            .find(|(k, _)| (*k).borrow() == key)
            .map(|(_, v)| v)
    }

    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: ?Sized + PartialEq,
    {
        self.data.iter().any(|(k, _)| k.borrow() == key)
    }

    /// Compares two maps for equality, ignoring insertion order and duplicate entries (the last
    /// value for a given key wins on both sides). This allocates two intermediate `HashMap`s, so
    /// it's exposed as a named method rather than [PartialEq]/[Eq], to keep that cost visible at
    /// the call site instead of hiding it behind `==`.
    pub fn slow_compare(&self, other: &Self) -> bool
    where
        K: Eq + Hash,
        V: PartialEq,
    {
        let lhs: HashMap<&K, &V> = self.data.iter().map(|(k, v)| (k, v)).collect();
        let rhs: HashMap<&K, &V> = other.data.iter().map(|(k, v)| (k, v)).collect();
        lhs == rhs
    }

    /// Remove all entries matching this key from the map. This method uses [Vec::retain], and is
    /// thus potentially costly (like any removal in a vector-like datastructure).
    // Note: we might implement a tombstone or option-based deletion later, if removal is a bit too
    // costly.
    #[inline]
    pub fn remove_slow<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: ?Sized + PartialEq,
    {
        self.data.retain(|(k, _)| k.borrow() != key);
    }

    /// Iterate over the element, including duplicate entries.
    #[inline]
    pub fn iter(&self) -> slice::Iter<'_, (K, V)> {
        self.data.iter()
    }

    /// Iterate mutably over the elements, including duplicate entries.
    #[inline]
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, (K, V)> {
        self.dirty();
        self.data.iter_mut()
    }

    /// Return the length of the underlying vector, thus including duplicate entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Return `true` if the map hasn't been extended since the last call to [Self::dedup],
    /// guaranteeing that the underlying vector doesn't have any duplicate key.
    ///
    /// If `is_deduped` returns `false`, the map may have duplicate keys.
    #[inline]
    pub fn is_deduped(&self) -> bool {
        self.deduped
    }

    /// Assert, without scanning, that this map holds no duplicate keys, setting the `deduped` flag.
    ///
    /// For builders whose source guarantees key uniqueness (e.g. msgpack decoding, where the wire
    /// format is a map), to skip the [Self::dedup] pass. A later mutation re-dirties the flag.
    ///
    /// **Caution**: if the source can actually contain duplicate keys, prefer [Self::dedup].
    #[inline]
    pub fn mark_deduped(&mut self) {
        self.deduped = true;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.data.clear()
    }

    #[inline]
    pub fn drain<R: std::ops::RangeBounds<usize>>(
        &mut self,
        range: R,
    ) -> std::vec::Drain<'_, (K, V)> {
        self.data.drain(range)
    }
}

impl<K: Eq + Hash, V> VecMap<K, V> {
    /// Returns a deduped map, that either borrows from `self` without performing any work if the
    /// map is already deduped, or dedup the entries in a new separate vec otherwise. As opposed to
    /// [Self::dedup], `as_deduped_map` takes an immutable reference to `self` but might allocate.
    /// Prefer [Self::dedup] when applicable.
    pub fn as_deduped_map(&self) -> DedupedVecMap<'_, K, V> {
        if self.deduped {
            DedupedVecMap::Borrowed(self)
        } else {
            DedupedVecMap::Owned(self.data.iter().map(|(k, v)| (k, v)).collect())
        }
    }

    /// This is a convenience wrapper around [Self::as_deduped_map] used in the msgpack encoder,
    /// where we expect the map to be deduped, but call `as_deduped_map` as a defensive measure. If
    /// the latter had to deduplicate and allocate a new vec, we log a warning (at most once).
    pub fn defensive_dedup(&self) -> DedupedVecMap<'_, K, V> {
        if !self.is_deduped() {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "VecMap not deduped before encoding. Performing defensive on-the-fly dedup"
                );
            }
        }

        self.as_deduped_map()
    }

    /// Remove entries with a duplicate key, only keeping the last one. After this, a flag is set
    /// internally, such that as long as the map isn't extended or mutably iterated, the next
    /// [Self::dedup] doesn't perform the work again.
    pub fn dedup(&mut self) {
        if self.deduped {
            return;
        }

        self.data.reverse();

        // Since we're going to shuffle elements around, it's not easy to keep references to keys in
        // the deduping set while deleting some of them, since deletion in a vec shifts all other
        // elements after it, invalidating references. When we finally call `retain`, we must not
        // hold any reference to vecmap elements anymore.
        //
        // The following approaches are possible:
        //
        // - clone the keys in the hashset. Alas, we don't want a `Clone` bound on `SpanText` (which
        //   is the type of keys in practice), as some representations can be expensive to clone,
        //   e.g. requiring to lock the GIL in Python (python-native, reference counted strings)
        // - a two-pass approach. In a first pass we store seen key references in a HashSet and
        //   build a bitmap of indices to keep. Once built, we can release the set and call
        //   `Vec::retain` without borrowing issues. It's safe but requires an additional pass over
        //   the vecmap and an auxiliary `Vec<bool>`, in addition to the hashset.
        // - an unsafe, one-pass approach: if we re-implement a custom `retain`, we can store key
        //   references in an auxiliary HashSet that are guaranteed to remain valid as we move
        //   elements: we first move an element to keep at their final location, and only then
        //   insert a pointer to the key in the `seen` hashmap, which will remain valid.
        //
        // We choose the two-pass approach, which is simpler, safe and reasonably fast. If needed in
        // the future, the unsafe one-pass approach can be implemented.
        let keep: Vec<bool> = {
            let mut seen = HashSet::with_capacity(self.len());
            self.data.iter().map(|(k, _)| seen.insert(k)).collect()
        };

        let mut keep = keep.into_iter();
        self.data.retain(|_| keep.next().unwrap_or(false));

        self.deduped = true;
    }
}

impl<K, V> From<Vec<(K, V)>> for VecMap<K, V> {
    fn from(data: Vec<(K, V)>) -> Self {
        Self {
            data,
            deduped: false,
        }
    }
}

impl<K, V> From<VecMap<K, V>> for Vec<(K, V)> {
    fn from(value: VecMap<K, V>) -> Self {
        value.data
    }
}

impl<K, V> FromIterator<(K, V)> for VecMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self {
            data: iter.into_iter().collect(),
            deduped: false,
        }
    }
}

impl<K, V> IntoIterator for VecMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a VecMap<K, V> {
    type Item = &'a (K, V);
    type IntoIter = slice::Iter<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut VecMap<K, V> {
    type Item = &'a mut (K, V);
    type IntoIter = slice::IterMut<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        // Since we iterate on keys as well, they can modified, and introduce duplicates.
        self.dirty();
        self.data.iter_mut()
    }
}

impl<K, V> Extend<(K, V)> for VecMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        self.dirty();
        self.data.extend(iter);
    }
}

impl<K: Serialize + Eq + Hash, V: Serialize> Serialize for VecMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let deduped = self.as_deduped_map();
        let mut map_ser = serializer.serialize_map(Some(deduped.len()))?;

        for (k, v) in deduped.iter() {
            map_ser.serialize_entry(k, v)?;
        }

        map_ser.end()
    }
}

pub enum DedupedVecMap<'a, K, V> {
    Borrowed(&'a VecMap<K, V>),
    Owned(HashMap<&'a K, &'a V>),
}

impl<'a, K, V> DedupedVecMap<'a, K, V> {
    #[inline]
    pub fn iter(&self) -> DedupedVecMapIter<'_, 'a, K, V> {
        match self {
            DedupedVecMap::Borrowed(vec_map) => DedupedVecMapIter::Borrowed(vec_map.iter()),
            DedupedVecMap::Owned(map) => DedupedVecMapIter::Owned(map.iter()),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            DedupedVecMap::Borrowed(vec_map) => vec_map.len(),
            DedupedVecMap::Owned(map) => map.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        match self {
            DedupedVecMap::Borrowed(vec_map) => vec_map.is_empty(),
            DedupedVecMap::Owned(map) => map.is_empty(),
        }
    }
}

pub enum DedupedVecMapIter<'b, 'a: 'b, K, V> {
    Borrowed(slice::Iter<'a, (K, V)>),
    Owned(std::collections::hash_map::Iter<'b, &'a K, &'a V>),
}

impl<'b, 'a: 'b, K, V> Iterator for DedupedVecMapIter<'b, 'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            DedupedVecMapIter::Borrowed(iter) => iter.next().map(|(k, v)| (k, v)),
            DedupedVecMapIter::Owned(iter) => iter.next().map(|(&k, &v)| (k, v)),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            DedupedVecMapIter::Borrowed(iter) => iter.size_hint(),
            DedupedVecMapIter::Owned(iter) => iter.size_hint(),
        }
    }
}

impl<'b, 'a: 'b, K, V> ExactSizeIterator for DedupedVecMapIter<'b, 'a, K, V> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_last_inserted() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("a", 2);
        assert_eq!(m.get("a"), Some(&2));
    }

    #[test]
    fn get_mut_returns_last_inserted() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("a", 2);
        *m.get_mut("a").unwrap() = 42;
        assert_eq!(m.get("a"), Some(&42));
        // First entry unchanged
        assert_eq!(m.iter().next().unwrap().1, 1);
    }

    #[test]
    fn remove_removes_all_occurrences() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("a", 3);
        m.remove_slow("a");
        assert_eq!(m.get("a"), None);
        assert!(!m.contains_key("a"));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn contains_key_works() {
        let mut m = VecMap::new();
        assert!(!m.contains_key("x"));
        m.insert("x", 10);
        assert!(m.contains_key("x"));
    }

    #[test]
    fn from_iterator() {
        let m: VecMap<&str, i32> = vec![("a", 1), ("b", 2)].into_iter().collect();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("b"), Some(&2));
    }

    #[test]
    fn into_iter_consuming() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        let pairs: Vec<_> = m.into_iter().collect();
        assert_eq!(pairs, vec![("a", 1), ("b", 2)]);
    }

    #[test]
    fn is_deduped_false_initially() {
        let m: VecMap<&str, i32> = VecMap::new();
        assert!(!m.is_deduped());
    }

    #[test]
    fn is_deduped_false_after_from() {
        let m: VecMap<&str, i32> = vec![("a", 1)].into();
        assert!(!m.is_deduped());
    }

    #[test]
    fn is_deduped_false_after_collect() {
        let m: VecMap<&str, i32> = vec![("a", 1)].into_iter().collect();
        assert!(!m.is_deduped());
    }

    #[test]
    fn dedup_sets_flag() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        assert!(!m.is_deduped());
        m.dedup();
        assert!(m.is_deduped());
    }

    #[test]
    fn dedup_on_empty_map() {
        let mut m: VecMap<String, i32> = VecMap::new();
        m.dedup();
        assert!(m.is_deduped());
        assert!(m.is_empty());
    }

    #[test]
    fn dedup_no_duplicates() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        m.dedup();
        assert_eq!(m.len(), 3);
        assert_eq!(m.get("a"), Some(&1));
        assert_eq!(m.get("b"), Some(&2));
        assert_eq!(m.get("c"), Some(&3));
    }

    #[test]
    fn dedup_keeps_last_value() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 10);
        m.insert("a", 2);
        m.insert("a", 3);
        m.insert("b", 20);
        m.dedup();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a"), Some(&3));
        assert_eq!(m.get("b"), Some(&20));
    }

    #[test]
    fn dedup_is_idempotent() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("a", 2);
        m.dedup();
        assert!(m.is_deduped());
        assert_eq!(m.len(), 1);
        m.dedup();
        assert!(m.is_deduped());
        assert_eq!(m.len(), 1);
        assert_eq!(m.get("a"), Some(&2));
    }

    #[test]
    fn insert_dirties_dedup_flag() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.dedup();
        assert!(m.is_deduped());

        m.insert("b", 2);
        assert!(!m.is_deduped());
    }

    #[test]
    fn extend_dirties_dedup_flag() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.dedup();
        assert!(m.is_deduped());

        m.extend(vec![("b", 2)]);
        assert!(!m.is_deduped());
    }

    #[test]
    fn iter_mut_dirties_dedup_flag() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.dedup();
        assert!(m.is_deduped());

        for (_, v) in m.iter_mut() {
            *v += 1;
        }

        assert!(!m.is_deduped());
    }

    #[test]
    fn deduped_vec_map_borrowed_iter_and_len() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.dedup();

        let d = m.defensive_dedup();
        assert!(matches!(d, DedupedVecMap::Borrowed(_)));
        assert_eq!(d.len(), 2);

        let mut items: Vec<_> = d.iter().collect();
        items.sort_by_key(|(k, _)| **k);
        assert_eq!(items, vec![(&"a", &1), (&"b", &2)]);
    }

    #[test]
    fn deduped_vec_map_copy_iter_and_len() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("a", 2);
        m.insert("b", 3);

        let d = m.defensive_dedup();
        assert!(matches!(d, DedupedVecMap::Owned(_)));
        assert_eq!(d.len(), 2);

        let items: HashMap<&&str, &i32> = d.iter().collect();
        assert_eq!(items[&"a"], &2);
        assert_eq!(items[&"b"], &3);
    }

    #[test]
    fn deduped_vec_map_iter_exact_size() {
        let mut m = VecMap::new();
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3);
        m.dedup();

        let d = m.defensive_dedup();
        let mut iter = d.iter();
        assert_eq!(iter.len(), 3);
        iter.next();
        assert_eq!(iter.len(), 2);
    }

    #[test]
    fn deduped_vec_map_empty() {
        let m: VecMap<String, i32> = VecMap::new();
        let d = DedupedVecMap::Borrowed(&m);
        assert_eq!(d.len(), 0);
        assert_eq!(d.iter().count(), 0);
    }

    #[test]
    fn dedup_does_not_require_clone() {
        #[derive(Debug, PartialEq, Eq, Hash)]
        struct NonCloneKey(u32);

        let mut m = VecMap::new();
        m.insert(NonCloneKey(1), "a");
        m.insert(NonCloneKey(2), "b");
        m.insert(NonCloneKey(1), "c");
        m.dedup();
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&NonCloneKey(1)), Some(&"c"));
        assert_eq!(m.get(&NonCloneKey(2)), Some(&"b"));
    }

    #[test]
    fn serialize_deduplicates_keeping_last() {
        let mut m = VecMap::new();
        m.insert("a", 0);
        m.insert("b", 0);
        m.insert("b", 1);
        m.insert("a", 1);
        m.insert("a", 3);
        m.insert("b", 2);

        let serialized: serde_json::Value = serde_json::to_value(&m).unwrap();
        let obj = serialized.as_object().unwrap();

        assert_eq!(obj.len(), 2);
        assert_eq!(obj.get("a").unwrap(), 3);
        assert_eq!(obj.get("b").unwrap(), 2);
    }
}
