// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::VecDeque, hash::Hash};

mod queuehasmpap {
    use hashbrown::{hash_table::HashTable, DefaultHashBuilder};
    use std::{
        collections::VecDeque,
        hash::{BuildHasher, Hash},
    };

    pub struct QueueHashMap<K, V> {
        table: HashTable<usize>,
        hash_builder: DefaultHashBuilder,
        items: VecDeque<(K, V)>,
        popped: usize,
    }

    impl<K, V> QueueHashMap<K, V>
    where
        K: PartialEq + Eq + Hash,
    {
        pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
            self.items.iter()
        }

        pub fn iter_idx(&self) -> impl Iterator<Item = usize> {
            self.popped..(self.popped + self.items.len())
        }

        pub fn len(&self) -> usize {
            self.items.len()
        }

        pub fn is_empty(&self) -> bool {
            self.items.is_empty()
        }

        // Remove the oldest item in the queue and return it
        pub fn pop_front(&mut self) -> Option<(K, V)> {
            let (k, v) = self.items.pop_front()?;
            let hash = make_hash(&self.hash_builder, &k);
            if let Ok(entry) = self.table.find_entry(hash, |&other| other == self.popped) {
                entry.remove();
            }
            debug_assert!(self.items.len() == self.table.len());
            self.popped += 1;
            Some((k, v))
        }

        pub fn get(&self, k: &K) -> Option<&V> {
            let hash = make_hash(&self.hash_builder, k);
            let idx = self
                .table
                .find(hash, |other| &self.items[other - self.popped].0 == k)?;
            Some(&self.items[*idx - self.popped].1)
        }

        pub fn get_idx(&self, idx: usize) -> Option<&(K, V)> {
            self.items.get(idx - self.popped)
        }

        pub fn get_mut_or_insert(&mut self, key: K, default: V) -> (&mut V, bool) {
            let hash = make_hash(&self.hash_builder, &key);
            if let Some(idx) = self
                .table
                .find(hash, |other| self.items[other - self.popped].0 == key)
            {
                return (&mut self.items[*idx - self.popped].1, false);
            }
            self.insert_nocheck(hash, key, default);

            #[allow(clippy::unwrap_used)]
            (&mut self.items.back_mut().unwrap().1, true)
        }

        pub fn insert(&mut self, key: K, value: V) -> (usize, bool) {
            let hash = make_hash(&self.hash_builder, &key);
            if let Some(idx) = self
                .table
                .find(hash, |other| self.items[other - self.popped].0 == key)
            {
                self.items[*idx - self.popped].1 = value;
                (*idx, false)
            } else {
                (self.insert_nocheck(hash, key, value), true)
            }
        }

        /// # Safety
        ///
        /// This function inserts a new item in the store unconditionnaly
        /// If the item already exists, it's drop implementation will not be called, and memory
        /// might leak
        ///
        /// The hash needs to be precomputed too
        fn insert_nocheck(&mut self, hash: u64, key: K, value: V) -> usize {
            let item_index = self.items.len() + self.popped;

            // Separate set and items since set is mutably borrowed, while items is unmutably
            let Self {
                table,
                items,
                popped,
                hash_builder,
                ..
            } = self;
            table.insert_unique(hash, item_index, |i| {
                make_hash(hash_builder, &items[i - *popped].0)
            });
            self.items.push_back((key, value));
            item_index
        }
    }

    impl<K, V> Default for QueueHashMap<K, V> {
        fn default() -> Self {
            Self {
                table: HashTable::new(),
                hash_builder: DefaultHashBuilder::default(),
                items: VecDeque::new(),
                popped: 0,
            }
        }
    }

    fn make_hash<T: Hash>(h: &DefaultHashBuilder, i: &T) -> u64 {
        h.hash_one(i)
    }
}

pub use queuehasmpap::QueueHashMap;

#[derive(Default)]
/// Stores telemetry data item, like dependencies and integrations
///
/// * Bounds the length of the collection it uses to prevent memory leaks
/// * Tries to keep a list of items that it has seen (within max number of items)
/// * Tries to keep a list of items that haven't been sent to datadog yet
/// * Deduplicates items, to make sure we don't send the item twice
pub struct Store<T> {
    // unflushed and set contain indices into
    unflushed: VecDeque<usize>,
    items: QueueHashMap<T, ()>,
    max_items: usize,
}

impl<T> Store<T>
where
    T: PartialEq + Eq + Hash,
{
    pub fn new(max_items: usize) -> Self {
        Self {
            unflushed: VecDeque::new(),
            items: QueueHashMap::default(),
            max_items,
        }
    }

    pub fn insert(&mut self, item: T) {
        if self.items.get(&item).is_some() {
            return;
        }
        if self.items.len() == self.max_items {
            self.items.pop_front();
        }
        let (idx, _) = self.items.insert(item, ());
        if self.unflushed.len() == self.max_items {
            self.unflushed.pop_front();
        }
        self.unflushed.push_back(idx);
    }

    // Reinsert all already flushed items in the flush queue
    pub fn unflush_stored(&mut self) {
        self.unflushed.clear();
        for i in self.items.iter_idx() {
            self.unflushed.push_back(i);
        }
    }

    // Remove the first `count` items in the queue
    pub fn removed_flushed(&mut self, count: usize) {
        for _ in 0..count {
            self.unflushed.pop_front();
        }
    }

    pub fn flush_not_empty(&self) -> bool {
        !self.unflushed.is_empty()
    }

    pub fn unflushed(&self) -> impl Iterator<Item = &T> {
        self.unflushed
            .iter()
            .flat_map(|i| Some(&self.items.get_idx(*i)?.0))
    }

    pub fn len_unflushed(&self) -> usize {
        self.unflushed.len()
    }

    pub fn len_stored(&self) -> usize {
        self.items.len()
    }
}

impl<T> Extend<T> for Store<T>
where
    T: PartialEq + Eq + Hash,
{
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for i in iter {
            self.insert(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smoke_insert() {
        let mut store = Store::new(10);
        store.insert("hello");
        store.insert("world");
        store.insert("world");

        assert_eq!(store.unflushed.len(), 2);
        assert_eq!(store.items.len(), 2);
        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&"hello", &"world"]);

        store.removed_flushed(1);
        assert_eq!(store.items.len(), 2);
        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&"world"]);

        store.removed_flushed(1);
        assert_eq!(store.items.len(), 2);
        assert!(store.unflushed().next().is_none());

        store.insert("hello");
        assert!(store.unflushed().next().is_none());
    }

    #[test]
    fn test_insert_spill() {
        let mut store = Store::new(5);
        for i in 2..15 {
            store.insert(i);
        }
        assert_eq!(store.unflushed.len(), 5);
        assert_eq!(store.items.len(), 5);

        assert_eq!(
            store.unflushed().collect::<Vec<_>>(),
            &[&10, &11, &12, &13, &14]
        )
    }

    #[test]
    fn test_insert_spill_no_unflush() {
        let mut store = Store::new(5);
        for i in 2..7 {
            store.insert(i);
        }
        assert_eq!(store.unflushed.len(), 5);

        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&2, &3, &4, &5, &6]);
        store.removed_flushed(4);

        for i in 7..10 {
            store.insert(i);
        }

        assert_eq!(store.unflushed.len(), 4);
        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&6, &7, &8, &9]);
    }

    #[test]
    fn test_unflush_stored() {
        let mut store = Store::new(5);
        for i in 2..7 {
            store.insert(i);
        }
        assert_eq!(store.unflushed.len(), 5);

        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&2, &3, &4, &5, &6]);
        store.unflush_stored();
        assert_eq!(store.unflushed().collect::<Vec<_>>(), &[&2, &3, &4, &5, &6]);
    }
}
