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
use std::collections::HashSet;
use std::hash::Hash;

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
#[derive(Clone, Debug, PartialEq)]
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
    pub fn iter(&self) -> std::slice::Iter<'_, (K, V)> {
        self.data.iter()
    }

    /// Iterate mutably over the elements, including duplicate entries.
    #[inline]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, (K, V)> {
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
}

impl<K: Hash + Eq + Clone, V> VecMap<K, V> {
    /// Remove entries with a duplicate key, only keeping the last one. After this, a flag is set
    /// internally, such that as long as the map isn't extended or mutably iterated, the next
    /// [Self::dedup] doesn't perform the work again.
    pub fn dedup(&mut self) {
        if self.deduped {
            return;
        }

        // Since we're going to shuffle elements around, it's not easy to keep references to keys in
        // the deduping set. The simplest is to clone them.
        let mut seen = HashSet::with_capacity(self.len());

        self.data.reverse();
        self.data.retain(|(k, _)| seen.insert(k.clone()));
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
    type IntoIter = std::slice::Iter<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut VecMap<K, V> {
    type Item = &'a mut (K, V);
    type IntoIter = std::slice::IterMut<'a, (K, V)>;

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
        use std::collections::HashMap;

        // We pre-compute the deduped map. If deduplication were done on the fly during
        // serialization, we couldn't provide a length up front to the serializer, and the current
        // one (rmp) will allocate an intermediate buffer defensively.
        if self.deduped {
            let mut map_ser = serializer.serialize_map(Some(self.len()))?;

            for (k, v) in self {
                map_ser.serialize_entry(k, v)?;
            }

            map_ser.end()
        } else {
            // Note: using `dedup` would need an additional `clone()` of the whole map here. We can
            // use references instead.
            self.data
                .iter()
                .map(|(k, v)| (k, v))
                // Since the iterator is sized, `collect()` should pre-allocate with the right
                // capacity in one shot.
                .collect::<HashMap<&K, &V>>()
                .serialize(serializer)
        }
    }
}

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
