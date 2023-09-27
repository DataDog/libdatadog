// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::*;
use bumpalo::Bump;
use ouroboros::self_referencing;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

#[cfg(test)]
use std::ops::Range;

struct BorrowedStringTable<'b> {
    /// The arena to store the characters in.
    arena: &'b Bump,

    /// Used to have efficient lookup by [StringId], and to provide an
    /// [Iterator] over the strings.
    vec: Vec<&'b str>,

    /// Used to have efficient lookup by [&str].
    map: HashMap<&'b str, StringId, BuildHasherDefault<rustc_hash::FxHasher>>,
}

#[self_referencing]
struct StringTableCell {
    // This arena holds the characters of the strings. The memory used to hold
    // the vec and map to implement the lookups required are not in the arena,
    // because they would be re-allocated over time but the previous data
    // wouldn't generally get reclaimed. This makes them a poor fit for an
    // arena allocator.
    owner: Bump,

    // This says that the BorrowedStringTable will hold a reference to the
    // field `owner`. Rust does not allow this as there are many ways to make
    // this code unsafe. The ouroboros crate is used to provide a safe
    // abstraction for a subset of self-referential behavior.
    #[borrows(owner)]
    #[covariant]
    dependent: BorrowedStringTable<'this>,
}

/// The [StringTable] stores strings and associates them with [StringId]s,
/// which correspond to the order in which strings were inserted. The empty
/// string is always associated with [StringId::ZERO].
pub struct StringTable {
    // ouroboros will add a lot of functions to this struct, which we don't
    // want to expose publicly, so the internals are wrapped and private.
    inner: StringTableCell,
}

impl StringTable {
    // Not guaranteed for a given system, but a very common size.
    const PAGE_SIZE: usize = 4096;

    // not guaranteed, has a test to double-check.
    const BUMP_OVERHEAD: usize = 64;

    /// A good initial capacity for the [StringTable]. Used by
    /// `[StringTable::new]` and its default impl.
    ///
    /// Ideally, we'd want the system allocator to allocate X pages. Pages are
    /// the granularity that operating systems typically give out memory,
    /// although some are definitely more efficient with larger number of
    /// pages, and some have a concept of huge pages. The point is, it's hard
    /// to totally generalize.
    ///
    /// However, we're not using a system allocator directly, we're using
    /// [Bump], which has some overhead. So we need to subtract off some
    /// overhead of whatever number we want, to avoid going into the next
    /// size. The good news is that it rounds up to page sizes of 4096, so we
    /// only need to get close.
    ///
    /// So, what remains is choosing how many pages to reserve up front. From
    /// one perspective, it would be nice to use a size that goes directly to
    /// `mmap` on Linux by a given malloc implementation, but if a certain
    /// number of profiles don't reach that number, than that's wasteful. We
    /// don't currently have metrics on this.
    ///
    /// So... for now, the selected number of pages is arbitrarily chosen.
    pub const GOOD_INITIAL_CAPACITY: usize = 8 * Self::PAGE_SIZE - Self::BUMP_OVERHEAD;

    #[inline]
    pub fn new() -> Self {
        Self::with_arena_capacity(Self::GOOD_INITIAL_CAPACITY)
    }

    /// Creates a new [StringTable] with an arena capacity of at least
    /// `capacity` in bytes. Keep in mind that the other structures will also
    /// use memory that is not included in this capacity.
    #[inline]
    fn with_arena_capacity(capacity: usize) -> Self {
        let arena = Bump::with_capacity(capacity);
        let inner = StringTableCell::new(arena, |arena| BorrowedStringTable {
            arena,
            vec: Default::default(),
            map: Default::default(),
        });

        let mut s = Self { inner };
        // string tables always have the empty string at 0.
        let (_id, _inserted) = s.insert_full("");
        debug_assert!(_id == StringId::ZERO);
        debug_assert!(_inserted);
        s
    }

    #[allow(unused)]
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.with_dependent(|table| table.vec.len())
    }

    #[allow(unused)]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Inserts the string into the table, if it did not already exist. The id
    /// of the string is returned.
    ///
    /// # Panics
    /// Panics if a new string needs to be inserted but the offset of the new
    /// string doesn't fit into a [StringId].
    #[inline]
    pub fn insert(&mut self, str: &str) -> StringId {
        self.insert_full(str).0
    }

    /// Inserts the string into the table, if it did not already exist. The id
    /// of the string is returned, along with whether the string was inserted.
    ///
    /// # Panics
    /// Panics if a new string needs to be inserted but the offset of the new
    /// string doesn't fit into a [StringId].
    #[inline]
    pub fn insert_full(&mut self, str: &str) -> (StringId, bool) {
        // For performance, delay converting the &str to a String until after
        // it has been determined to not exist in the set. This avoids
        // temporary allocations.
        self.inner
            .with_dependent_mut(|table| match table.map.get(str) {
                None => {
                    let id = StringId::from_offset(table.vec.len());
                    let bumped_str = table.arena.alloc_str(str);

                    table.vec.push(bumped_str);
                    table.map.insert(bumped_str, id);
                    assert_eq!(table.vec.len(), table.map.len());
                    (id, true)
                }
                Some(id) => (*id, false),
            })
    }

    /// Gets the string associated with the id.
    ///
    /// # Panics
    /// Panics if the [StringId] doesn't exist in the table.
    #[inline]
    pub fn get_id(&self, id: StringId) -> &str {
        self.inner.with_dependent(|table| {
            let offset = id.to_offset();
            match table.vec.get(offset) {
                Some(str) => str,
                None => panic!("expected string id {offset} to exist in the string table"),
            }
        })
    }

    #[cfg(test)]
    #[allow(unused)]
    #[inline]
    pub fn get_range(&self, range: Range<usize>) -> &[&str] {
        self.inner.with_dependent(|table| &table.vec[range])
    }

    /// Returns an iterator over the strings in the table. The items are
    /// returned in the order they were inserted, matching the [StringId]s.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.inner.with_dependent(|table| table.vec.iter().copied())
    }
}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// If this fails, Bumpalo may have changed its allocation patterns, and
    /// [StringTable::new] and [StringTable::GOOD_INITIAL_CAPACITY] may need
    /// adjusted. The test's purpose is to ensure that amount of memory
    /// actually returned by Bumpalo matches our expectations.
    #[test]
    fn test_bump() {
        const BUMP_OVERHEAD: u64 = StringTable::BUMP_OVERHEAD as u64;
        let given_capacity = StringTable::GOOD_INITIAL_CAPACITY as u64;
        let actual_capacity = given_capacity.next_power_of_two() - BUMP_OVERHEAD;
        let arena = Bump::with_capacity(given_capacity as usize);
        assert_eq!(actual_capacity as usize, arena.chunk_capacity());
    }

    #[test]
    fn owned_string_table() {
        // small size, to allow testing re-alloc.
        // todo: actually alloc more than this capacity due to Bump's rounding.
        let mut set = StringTable::with_arena_capacity(64);

        // the empty string must always be included in the set at 0.
        let empty_str = set.get_id(StringId::ZERO);
        assert_eq!("", empty_str);

        let cases: &[_] = &[
            (StringId::ZERO, ""),
            (StringId::from_offset(1), "local root span id"),
            (StringId::from_offset(2), "span id"),
            (StringId::from_offset(3), "trace endpoint"),
            (StringId::from_offset(4), "samples"),
            (StringId::from_offset(5), "count"),
            (StringId::from_offset(6), "wall-time"),
            (StringId::from_offset(7), "nanoseconds"),
            (StringId::from_offset(8), "cpu-time"),
            (StringId::from_offset(9), "<?php"),
            (StringId::from_offset(10), "/srv/demo/public/index.php"),
            (StringId::from_offset(11), "pid"),
        ];

        for (offset, str) in cases.iter() {
            let actual_offset = set.insert(str);
            assert_eq!(*offset, actual_offset);
        }

        // repeat them to ensure they aren't re-added
        for (offset, str) in cases.iter() {
            let actual_offset = set.insert(str);
            assert_eq!(*offset, actual_offset);
        }

        // let's fetch some offsets
        assert_eq!("", set.get_id(StringId::ZERO));
        assert_eq!(
            "/srv/demo/public/index.php",
            set.get_id(StringId::from_offset(10))
        );

        // Check a range too
        let slice = set.get_range(7..10);
        let expected_slice = &["nanoseconds", "cpu-time", "<?php"];
        assert_eq!(expected_slice, slice);

        // And the whole set:
        assert_eq!(cases.len(), set.len());
        let actual = set
            .iter()
            .enumerate()
            .map(|(offset, item)| (StringId::from_offset(offset), item))
            .collect::<Vec<_>>();
        assert_eq!(cases, &actual);
    }
}
