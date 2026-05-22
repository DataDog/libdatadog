// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module defines a associative map datastructure for spans data (meta, metrics, etc.) backed
//! by a vector. Spans are mostly allocated and constructed, and more rarely read or mutated.
//! [VecMap] is thus optimized for insertion (which is just `Vec::push`), without any hashing
//! involved. Fetching and removing a value is, on the other hand, linear time in the size of the
//! map.

use serde::ser::{Serialize, SerializeMap, Serializer};
use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::Hash;

/// A Vec-backed map that provides HashMap-like lookup by key.
///
/// # Duplicates
///
/// Duplicates are tolerated: [VecMap::insert] always appends, and [VecMap::get]/[VecMap::get_mut]
/// return the *last* matching entry so that later writes shadow earlier ones. This optimizes for
/// fast insert and construction (that might happen on the client's application hot path), avoiding
/// a linear scan on each insert (or a potential costly full re-hashing with a hashmap).
/// Additionally, while overriding a metric or a meta definitively happens, it's assumed to be rare
/// enough so such that the size penalty of duplication is expected to be reasonable.
///
/// **Important**: note that only [VecMap::get] and [VecMap::get_mut] are duplicate-aware, so to
/// speak. [Vec::len], [Vec::iter], and others just delegates to the underlying `Vec`, and won't
/// deduplicate.
///
/// Explicit deduplication is currently being done automatically and on-the-fly during
/// serialization. If needed, in the future, we might trigger deduplication on other events, for
/// example at insertion if the size is bigger than a threshold.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct VecMap<K, V>(Vec<(K, V)>);

impl<K, V> VecMap<K, V> {
    #[must_use]
    #[inline]
    pub fn new() -> Self {
        VecMap(Vec::new())
    }

    #[must_use]
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        VecMap(Vec::with_capacity(capacity))
    }

    #[inline]
    pub fn insert(&mut self, key: K, value: V) {
        self.0.push((key, value));
    }

    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: ?Sized + PartialEq,
    {
        self.0
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
        self.0
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
        self.0.iter().any(|(k, _)| k.borrow() == key)
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
        self.0.retain(|(k, _)| k.borrow() != key);
    }

    /// Iterate over the element, including duplicate entries.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, (K, V)> {
        self.0.iter()
    }

    /// Iterate mutably over the elements, including duplicate entries.
    #[inline]
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, (K, V)> {
        self.0.iter_mut()
    }

    /// Return the length of the underlying vector, thus including duplicate entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<K, V> From<Vec<(K, V)>> for VecMap<K, V> {
    fn from(vec: Vec<(K, V)>) -> Self {
        VecMap(vec)
    }
}

impl<K, V> FromIterator<(K, V)> for VecMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        VecMap(iter.into_iter().collect())
    }
}

impl<K, V> IntoIterator for VecMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a VecMap<K, V> {
    type Item = &'a (K, V);
    type IntoIter = std::slice::Iter<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut VecMap<K, V> {
    type Item = &'a mut (K, V);
    type IntoIter = std::slice::IterMut<'a, (K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

impl<K, V> Extend<(K, V)> for VecMap<K, V> {
    fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

impl<K: Serialize + Eq + Hash, V: Serialize> Serialize for VecMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        let mut seen = HashSet::with_capacity(self.len());
        for (k, v) in self.0.iter().rev() {
            if seen.insert(k) {
                map.serialize_entry(k, v)?;
            }
        }
        map.end()
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
