// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{borrow::Borrow, fmt, marker::PhantomData};
use serde::{Serialize, Serializer};

/// Key-value pairs stored as a list and serialized as a map.
///
/// Lookups are linear, which suits the handful of entries telemetry maps hold. Reading and
/// serializing only need `S: AsRef<[(K, V)]>`, so arrays and borrowed slices work without
/// allocation. [`ListMap::insert`] additionally needs [`PushStorage`], which `Vec` implements
/// and fixed-capacity vectors can implement.
///
/// Keys passed in through [`ListMap::new`] must be unique; serialization does not deduplicate
/// them. [`ListMap::insert`] keeps keys unique by replacing existing entries.
pub struct ListMap<K, V, S> {
    entries: S,
    _entry: PhantomData<fn() -> (K, V)>,
}

/// Storage that can grow one entry at a time, such as `Vec` or a fixed-capacity vector.
pub trait PushStorage<T>: AsRef<[T]> + AsMut<[T]> {
    /// Appends `value`, handing it back if the storage is full.
    fn push(&mut self, value: T) -> Result<(), T>;

    /// Removes all entries.
    fn clear(&mut self);
}

impl<K, V, S> ListMap<K, V, S> {
    /// Wraps `entries`, whose keys must be unique.
    pub const fn new(entries: S) -> Self {
        Self {
            entries,
            _entry: PhantomData,
        }
    }

    /// Returns the underlying storage.
    pub fn into_inner(self) -> S {
        self.entries
    }
}

impl<K, V, S: AsRef<[(K, V)]>> ListMap<K, V, S> {
    /// Returns the value stored for `key`.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: PartialEq + ?Sized,
    {
        self.iter()
            .find(|(k, _)| (*k).borrow() == key)
            .map(|(_, v)| v)
    }

    /// Iterates over the entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.as_ref().iter().map(|(k, v)| (k, v))
    }

    /// Iterates over the keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.entries.as_ref().len()
    }

    /// Returns `true` if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.as_ref().is_empty()
    }
}

impl<K: PartialEq, V, S: PushStorage<(K, V)>> ListMap<K, V, S> {
    /// Inserts `value` for `key`, returning the previous value if `key` was present.
    ///
    /// # Errors
    ///
    /// Returns the entry back if `key` is new and the storage is full.
    pub fn insert(&mut self, key: K, value: V) -> Result<Option<V>, (K, V)> {
        match self.entries.as_mut().iter_mut().find(|(k, _)| *k == key) {
            Some((_, existing)) => Ok(Some(core::mem::replace(existing, value))),
            None => self.entries.push((key, value)).map(|()| None),
        }
    }

    /// Removes all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl<K, V, S: Clone> Clone for ListMap<K, V, S> {
    fn clone(&self) -> Self {
        Self::new(self.entries.clone())
    }
}

impl<K, V, S: Default> Default for ListMap<K, V, S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}

impl<K, V, S: fmt::Debug> fmt::Debug for ListMap<K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entries.fmt(f)
    }
}

impl<K, V, S: PartialEq> PartialEq for ListMap<K, V, S> {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl<K: Serialize, V: Serialize, S: AsRef<[(K, V)]>> Serialize for ListMap<K, V, S> {
    fn serialize<Ser: Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        serializer.collect_map(self.iter())
    }
}

#[cfg(feature = "alloc")]
mod vec_storage {
    use alloc::vec::Vec;

    use super::{ListMap, PushStorage};

    impl<T> PushStorage<T> for Vec<T> {
        fn push(&mut self, value: T) -> Result<(), T> {
            Vec::push(self, value);
            Ok(())
        }

        fn clear(&mut self) {
            Vec::clear(self);
        }
    }

    impl<K: PartialEq, V> FromIterator<(K, V)> for ListMap<K, V, Vec<(K, V)>> {
        /// Collects entries, keeping the last value for repeated keys.
        fn from_iter<I: IntoIterator<Item = (K, V)>>(entries: I) -> Self {
            let mut map = Self::default();
            for (key, value) in entries {
                // `Vec` storage never runs out of capacity.
                let _ = map.insert(key, value);
            }
            map
        }
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn serializes_array_storage_as_map() {
        let map = ListMap::new([("appsec", 1), ("profiler", 2)]);
        assert_eq!(map.get("profiler"), Some(&2));
        assert_eq!(
            serde_json::to_value(map).unwrap(),
            serde_json::json!({"appsec": 1, "profiler": 2})
        );
    }

    #[test]
    fn insert_replaces_existing_keys() {
        let mut map = ListMap::<_, _, Vec<_>>::default();
        assert_eq!(map.insert("appsec", 1), Ok(None));
        assert_eq!(map.insert("appsec", 2), Ok(Some(1)));
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("appsec"), Some(&2));

        let collected: ListMap<_, _, Vec<_>> = [("a", 1), ("b", 2), ("a", 3)].into_iter().collect();
        assert_eq!(collected.into_inner(), [("a", 3), ("b", 2)]);
    }

    #[test]
    fn insert_reports_full_storage() {
        type Entry = (&'static str, u8);

        /// Storage with room for a single entry.
        struct One(Option<Entry>);
        impl AsRef<[Entry]> for One {
            fn as_ref(&self) -> &[Entry] {
                self.0.as_slice()
            }
        }
        impl AsMut<[Entry]> for One {
            fn as_mut(&mut self) -> &mut [Entry] {
                self.0.as_mut_slice()
            }
        }
        impl PushStorage<Entry> for One {
            fn push(&mut self, value: Entry) -> Result<(), Entry> {
                match self.0 {
                    Some(_) => Err(value),
                    None => {
                        self.0 = Some(value);
                        Ok(())
                    }
                }
            }
            fn clear(&mut self) {
                self.0 = None;
            }
        }

        let mut map = ListMap::new(One(None));
        assert_eq!(map.insert("a", 1), Ok(None));
        assert_eq!(map.insert("a", 2), Ok(Some(1)));
        assert_eq!(map.insert("b", 3), Err(("b", 3)));
    }
}
