// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::collections::alloc::Global;
use core::cell::{Cell, RefCell};
use rust_alloc::format;
use rust_alloc::rc::Rc;
use rust_alloc::string::String;
use rust_alloc::vec::Vec;

/// Forwards to `Global`, records live blocks, and can be told to fail.
#[derive(Default)]
struct Tracking {
    live: RefCell<Vec<(usize, Layout)>>,
    allocations: Cell<usize>,
    /// Allocations still allowed before failing; `None` means unlimited.
    budget: Cell<Option<usize>>,
}

impl Tracking {
    fn take_budget(&self) -> Result<(), AllocError> {
        match self.budget.get() {
            Some(0) => Err(AllocError),
            Some(n) => {
                self.budget.set(Some(n - 1));
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn record(&self, block: NonNull<[u8]>, layout: Layout) -> NonNull<[u8]> {
        self.allocations.set(self.allocations.get() + 1);
        self.live
            .borrow_mut()
            .push((block.cast::<u8>().as_ptr().addr(), layout));
        block
    }

    fn forget(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut live = self.live.borrow_mut();
        let at = live
            .iter()
            .position(|&(addr, _)| addr == ptr.as_ptr().addr())
            .expect("freeing a block this allocator did not hand out");
        assert_eq!(
            live.swap_remove(at).1,
            layout,
            "freed with a different layout"
        );
    }
}

// SAFETY: forwards to `Global`; bookkeeping never touches the blocks.
unsafe impl Allocator for Tracking {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.take_budget()?;
        Ok(self.record(Global.allocate(layout)?, layout))
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.take_budget()?;
        Ok(self.record(Global.allocate_zeroed(layout)?, layout))
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.forget(ptr, layout);
        // SAFETY: the caller's guarantees carry over to `Global`.
        unsafe { Global.deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.take_budget()?;
        // SAFETY: the caller's guarantees carry over to `Global`.
        let block = unsafe { Global.grow(ptr, old, new)? };
        self.forget(ptr, old);
        Ok(self.record(block, new))
    }
}

type Map<'t, K, V> = IndexMap<K, V, &'t Tracking>;

#[test]
fn inserts_find_and_keep_insertion_order() {
    let alloc = Tracking::default();
    // Enough keys to grow the table several times; fewer under Miri.
    let count = if cfg!(miri) { 300 } else { 5000 };
    let keys: Vec<String> = (0..count).map(|i| format!("key-{i}")).collect();
    let mut map: Map<'_, &str, usize> = IndexMap::new_in(&alloc);
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(map.try_insert(key, i * 10), Ok((i, None)));
    }
    assert_eq!(map.len(), keys.len());
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(map.get(key.as_str()), Some(&(i * 10)));
        assert_eq!(map.get_index_of(key.as_str()), Some(i));
        assert_eq!(map.get_index(i), Some((&key.as_str(), &(i * 10))));
    }
    assert_eq!(map.get("missing"), None);
    assert!(
        map.iter()
            .map(|(k, _)| *k)
            .eq(keys.iter().map(String::as_str))
    );
    assert!(
        map.keys()
            .rev()
            .copied()
            .eq(keys.iter().rev().map(String::as_str))
    );
    assert_eq!(map.values().len(), keys.len());
    drop(map);
    assert!(alloc.live.borrow().is_empty(), "everything is freed");
}

#[test]
fn duplicate_keys_keep_their_index() {
    let mut map = IndexMap::<&str, u32, Global>::new_in(Global);
    assert_eq!(map.try_insert("a", 1), Ok((0, None)));
    assert_eq!(map.try_insert("b", 2), Ok((1, None)));
    assert_eq!(map.try_insert("a", 3), Ok((0, Some(1))));
    assert_eq!(map.len(), 2);
    assert_eq!(map.get_full("a"), Some((0, &"a", &3)));
    *map.get_mut("b").unwrap() += 40;
    assert_eq!(map.get("b"), Some(&42));
    assert!(map.contains_key("b"));
    assert!(!map.contains_key("c"));
}

#[test]
fn entry_api() {
    let mut map = IndexMap::<String, u32, Global>::new_in(Global);
    match map.try_entry(String::from("x")).unwrap() {
        Entry::Vacant(entry) => {
            assert_eq!(entry.index(), 0);
            assert_eq!(entry.key(), "x");
            *entry.insert(1) += 1;
        }
        Entry::Occupied(_) => panic!("new key"),
    }
    match map.try_entry(String::from("x")).unwrap() {
        Entry::Occupied(mut entry) => {
            assert_eq!(
                (entry.index(), entry.key().as_str(), *entry.get()),
                (0, "x", 2)
            );
            *entry.get_mut() = 7;
        }
        Entry::Vacant(_) => panic!("existing key"),
    }
    // Lookups by &str work for String keys.
    assert_eq!(map.get("x"), Some(&7));
}

#[test]
fn dropped_vacant_entry_changes_nothing() {
    let mut map = IndexMap::<u64, (), Global>::new_in(Global);
    map.try_insert(1, ()).unwrap();
    {
        let entry = map.try_entry(2).unwrap();
        assert_eq!(entry.index(), 1);
    }
    assert_eq!(map.len(), 1);
    assert!(!map.contains_key(&2));
    let Entry::Vacant(entry) = map.try_entry(3).unwrap() else {
        panic!("new key")
    };
    assert_eq!(entry.into_key(), 3);
    assert_eq!(map.try_insert(4, ()), Ok((1, None)));
}

#[test]
fn reserve_avoids_later_allocations() {
    let alloc = Tracking::default();
    let mut map: Map<'_, u64, u64> = IndexMap::try_with_capacity_in(1000, &alloc).unwrap();
    assert!(map.capacity() >= 1000);
    let before = alloc.allocations.get();
    for i in 0..1000 {
        map.try_insert(i, i).unwrap();
    }
    assert_eq!(
        alloc.allocations.get(),
        before,
        "no allocation within capacity"
    );
    assert!(map.try_reserve(usize::MAX).is_err());
    assert!(map.try_reserve(u32::MAX as usize).is_err());
}

#[test]
fn allocation_failure_leaves_the_map_unchanged() {
    let alloc = Tracking::default();
    let mut map: Map<'_, u64, u64> = IndexMap::new_in(&alloc);
    for budget in 0..4 {
        alloc.budget.set(Some(budget));
        // Fill until something must allocate and the budget runs out.
        let mut next = map.len() as u64;
        loop {
            let len = map.len();
            match map.try_insert(next, next * 2) {
                Ok((index, None)) => assert_eq!(index, len),
                Ok(other) => panic!("unexpected {other:?}"),
                Err(AllocError) => {
                    assert_eq!(map.len(), len);
                    assert!(!map.contains_key(&next));
                    break;
                }
            }
            next += 1;
        }
        for key in 0..next {
            assert_eq!(map.get(&key), Some(&(key * 2)));
        }
        // Existing keys never need to allocate, even with no budget left.
        alloc.budget.set(Some(0));
        for key in 0..next {
            assert_eq!(
                map.try_insert(key, key * 2),
                Ok((usize::try_from(key).unwrap(), Some(key * 2)))
            );
        }
        alloc.budget.set(None);
    }
    drop(map);
    assert!(alloc.live.borrow().is_empty());
}

#[test]
fn values_and_keys_are_dropped_once() {
    struct Counted(Rc<Cell<usize>>);
    impl Drop for Counted {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let drops = Rc::new(Cell::new(0));
    let mut map = IndexMap::<u32, Counted, Global>::new_in(Global);
    for i in 0..100 {
        map.try_insert(i, Counted(drops.clone())).unwrap();
    }
    // A replaced value is handed back, then dropped here.
    let (_, old) = map.try_insert(5, Counted(drops.clone())).unwrap();
    drop(old);
    assert_eq!(drops.get(), 1);
    drop(map);
    assert_eq!(drops.get(), 101);
}

#[test]
fn empty_map_allocates_nothing() {
    let alloc = Tracking::default();
    let map: Map<'_, u64, u64> = IndexMap::new_in(&alloc);
    assert!(map.is_empty());
    assert_eq!(map.capacity(), 0);
    assert_eq!(map.get(&1), None);
    assert_eq!(map.iter().count(), 0);
    drop(map);
    assert_eq!(alloc.allocations.get(), 0);
}

#[test]
fn seeded_hasher_gives_the_same_results() {
    let mut map = IndexMap::with_hasher_in(DefaultHashBuilder::with_seed(0xdead_beef), Global);
    for i in 0..300_u32 {
        map.try_insert(i, i + 1).unwrap();
    }
    assert!((0..300).all(|i| map.get(&i) == Some(&(i + 1))));
    assert_eq!(
        format!("{:?}", map.iter().take(2).collect::<Vec<_>>()),
        "[(0, 1), (1, 2)]"
    );
}

#[test]
fn default_hasher_spreads_tag_and_group_bits() {
    // HashTable takes its tag from the top 7 bits and its start group from
    // the low 57; both must vary with the key.
    let builder = DefaultHashBuilder::default();
    let mut tags = [false; 128];
    let mut tops = [false; 128];
    for i in 0_u64..4096 {
        let hash = builder.hash_one(i);
        tags[usize::try_from(hash & 0x7f).unwrap()] = true;
        tops[usize::try_from(hash >> 57).unwrap()] = true;
    }
    assert!(tags.iter().all(|&seen| seen), "every 7-bit tag occurs");
    assert!(
        tops.iter().all(|&seen| seen),
        "every top-7-bit value occurs"
    );
}

#[test]
fn equivalent_finds_composite_keys_without_owning_them() {
    /// Borrowed form of a `(String, String)` key. It hashes like the tuple,
    /// which hashes its fields in order.
    #[derive(Hash)]
    struct Pair<'a>(&'a str, &'a str);

    impl Equivalent<(String, String)> for Pair<'_> {
        fn equivalent(&self, key: &(String, String)) -> bool {
            self.0 == key.0 && self.1 == key.1
        }
    }

    let mut map = IndexMap::<(String, String), u32, Global>::new_in(Global);
    map.try_insert((String::from("service"), String::from("web")), 1)
        .unwrap();
    map.try_insert((String::from("env"), String::from("prod")), 2)
        .unwrap();
    assert_eq!(map.get(&Pair("env", "prod")), Some(&2));
    assert_eq!(map.get_index_of(&Pair("service", "web")), Some(0));
    assert_eq!(map.get(&Pair("env", "web")), None);
}
