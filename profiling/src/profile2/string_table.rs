// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use ahash::RandomState;
use bumpalo::collections::String;
use bumpalo::Bump;
use std::collections::HashMap;
use std::ops::Index;

/// StringTable logically keeps a set of unique strings and provides an
/// integer-based identifier to refer to a unique string. The empty string
/// always has id 0, and ids will be incremented sequentially from there based
/// on the insertion order. These ids can index and slice into the table.
///
///     use bumpalo::Bump;
///     use datadog_profiling::profile2::StringTable;
///     let arena = Bump::new();
///     // Safety: the arena is not modified outside of the string table.
///     let mut table = unsafe { StringTable::new(&arena) };
///     let empty_str = table[0]; // safe, always exists.
///
///     let lrsi_id = table.insert("local root span id");
///     assert_eq!(lrsi_id, 1);
///     let si_id = table.insert("span id");
///     assert_eq!(si_id, 2);
///
///     let expected_slice = &["local root span id", "span id"];
///     let actual_slice = &table[1..=2];
///     assert_eq!(expected_slice, actual_slice);
///
pub struct StringTable<'s> {
    arena: &'s Bump,
    strings: Vec<&'s str>,
    set: HashMap<&'s str, usize, RandomState>,
}

impl<'s> StringTable<'s> {
    /// The current number of strings held in the string table.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    /// # Safety
    /// The arena must be treated as if the string table owns it!
    /// Do not allocate any data to the arena outside of the string table.
    /// Do not reset the arena until the string table is gone.
    pub unsafe fn new(arena: &'s Bump) -> Self {
        let empty_str = String::new_in(arena);
        let bumped_str = empty_str.into_bump_str();

        /// The initial size of the Vec. At the time of writing, Vec would
        /// choose size 4. This is expected to be much too small for the
        /// use-case, so use a larger initial capacity to save a few
        /// re-allocations in the beginning.
        /// This is just an educated estimate, not a finely tuned value.
        const INITIAL_VEC_CAPACITY: usize = 1024 / std::mem::size_of::<&str>();

        /// A HashMap is less straight-forward, but it uses more memory for
        /// the same number of elements compared to a Vec, but not twice as
        /// much for our situation, so dividing by 2 should be okay, at least
        /// until further measurement is done.
        const INITIAL_MAP_CAPACITY: usize = INITIAL_VEC_CAPACITY / 2;

        let mut strings = Vec::with_capacity(INITIAL_VEC_CAPACITY);
        strings.push(bumped_str);
        let mut set = HashMap::with_capacity_and_hasher(INITIAL_MAP_CAPACITY, Default::default());
        set.insert(bumped_str, usize::default());
        Self {
            arena,
            strings,
            set,
        }
    }

    pub fn insert_full(&mut self, str: &str) -> (usize, bool) {
        match self.set.get(str) {
            None => {
                let owned = String::from_str_in(str, self.arena);

                /* Consume the string but retain a reference to its data in
                 * the arena. The reference is valid as long as the arena
                 * doesn't get reset. This is partly the reason for the unsafe
                 * marker on `StringTable::new`.
                 */
                let bumped_str = owned.into_bump_str();

                let id = self.strings.len();
                self.strings.push(bumped_str);

                self.set.insert(bumped_str, id);
                assert_eq!(self.set.len(), self.strings.len());
                (id, true)
            }
            Some(offset) => (*offset, false),
        }
    }

    pub fn insert(&mut self, str: &str) -> usize {
        self.insert_full(str).0
    }
}

impl<'s, Idx> Index<Idx> for StringTable<'s>
where
    Idx: std::slice::SliceIndex<[&'s str]>,
{
    type Output = Idx::Output;

    fn index(&self, index: Idx) -> &Self::Output {
        &self.strings[index]
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    pub fn basic() {
        let arena = Bump::with_capacity(16);

        // Safety: the arena is left alone as required.
        let mut table = unsafe { StringTable::new(&arena) };

        let empty_str = table[0];
        assert_eq!("", empty_str);

        let cases = &[
            (0, ""),
            (1, "local root span id"),
            (2, "span id"),
            (3, "trace endpoint"),
            (4, "samples"),
            (5, "count"),
            (6, "wall-time"),
            (7, "nanoseconds"),
            (8, "cpu-time"),
            (9, "<?php"),
            (10, "/srv/demo/public/index.php"),
            (11, "pid"),
        ];

        for (offset, str) in cases.iter() {
            let actual_offset = table.insert(str);
            assert_eq!(*offset, actual_offset);
        }

        // repeat them to ensure they aren't re-added
        for (offset, str) in cases.iter() {
            let actual_offset = table.insert(str);
            assert_eq!(*offset, actual_offset);
        }

        // let's fetch some offsets
        assert_eq!("", table[0]);
        assert_eq!("/srv/demo/public/index.php", table[10]);

        // Check a range too
        let slice = &table[7..=9];
        let expected_slice = &["nanoseconds", "cpu-time", "<?php"];
        assert_eq!(expected_slice, slice);
    }
}
