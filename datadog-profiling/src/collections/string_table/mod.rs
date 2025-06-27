// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[allow(unused)]
pub mod wordpress_test_data;

use crate::iter::{IntoLendingIterator, LendingIterator};
use datadog_alloc::{AllocError, Allocator, ChainAllocator, VirtualAllocator};
use datadog_profiling_protobuf::StringOffset;
use std::alloc::Layout;
use std::mem;

/// A trait that indicates an allocator is arena allocator, meaning it doesn't
/// deallocate individual items, but deallocates their memory as a group when
/// the arena is dropped.
pub trait ArenaAllocator: Allocator {
    /// Copies the str into the arena, and returns a slice to the new str.
    fn allocate_slice<T: Copy + Sized>(&self, slice: &[T]) -> Result<&[T], AllocError> {
        // TODO: We might want each allocator to return its own empty slice
        // so we can debug where the value came from.
        if slice.is_empty() {
            return Ok(&[]);
        }
        let layout = Layout::for_value(slice);
        let uninit_ptr = Allocator::allocate(self, layout)?.cast::<T>();

        // Copy the bytes of the string into the allocated memory.
        // SAFETY: this is guaranteed to not be overlapping because an
        // allocator must not return aliasing bytes in its allocations.
        unsafe {
            let src = slice.as_ptr();
            let dst = uninit_ptr.as_ptr();
            let count = slice.len();
            core::ptr::copy_nonoverlapping(src, dst, count);
        }

        // SAFETY: The bytes were properly initialized, and they cannot be
        // misaligned because they have an alignment of 1, so it is safe to
        // create a slice of the given data and length. The lifetime matches
        // the arena allocator's lifetime.
        Ok(unsafe { core::slice::from_raw_parts(uninit_ptr.as_ptr(), slice.len()) })
    }

    fn allocate_str(&self, string: &str) -> Result<&str, AllocError> {
        // SAFETY: copied utf8 without modification, must also be valid utf8.
        Ok(unsafe { core::str::from_utf8_unchecked(self.allocate_slice(string.as_bytes())?) })
    }
}

impl<A: Allocator + Clone> ArenaAllocator for ChainAllocator<A> {}

type Hasher = core::hash::BuildHasherDefault<rustc_hash::FxHasher>;
type HashSet<K> = indexmap::IndexSet<K, Hasher>;

/// Holds unique strings and provides [`StringOffset`]s that correspond to the
/// order that the strings were inserted.
pub struct StringTable {
    /// The bytes of each string stored in `strings` are allocated here.
    bytes: ChainAllocator<VirtualAllocator>,

    /// The ordered hash set of unique strings. The order becomes the
    /// [`StringOffset`]. The static lifetime is a lie, it is tied to the
    /// `bytes`, which is only moved if the string table is moved e.g.
    /// [`StringTable::into_lending_iterator`].
    /// References to the underlying strings should generally not be handed,
    /// but if they are, they should be bound to the string table's lifetime
    /// or the lending iterator's lifetime.
    strings: HashSet<&'static str>,
}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, thiserror::Error)]
pub enum StringTableError {
    #[error("string table failed: invalid input")]
    InvalidInput,
    #[error("string table lookup failed: offset not found")]
    NotFound,
    #[error("string table insertion failed: out of memory")]
    OutOfMemory,
    #[error("string table insertion failed: storage full")]
    StorageFull,
}

impl StringTable {
    // Keep in mind 32-bit .NET. There is only 2 GiB of virtual memory total
    // available to an application, and we're not the application, we're just
    // a piece inside it. Additionally, there may be 2 or more string tables
    // in memory at a given time. Talk to .NET profiling engineers before
    // making this any bigger.
    const ARENA_SIZE_HINT: usize = 4 * 1024 * 1024;

    /// Well-known string offsets for commonly used strings
    pub const END_TIMESTAMP_NS_OFFSET: StringOffset = StringOffset::new(1);
    pub const LOCAL_ROOT_SPAN_ID_OFFSET: StringOffset = StringOffset::new(2);
    pub const TRACE_ENDPOINT_OFFSET: StringOffset = StringOffset::new(3);
    pub const SPAN_ID_OFFSET: StringOffset = StringOffset::new(4);

    /// Number of well-known strings: "", "end_timestamp_ns", "local root span id", "trace
    /// endpoint", "span id"
    pub const WELL_KNOWN_COUNT: usize = 5;

    /// Returns true if the given offset refers to a well-known string.
    /// Well-known strings are: "", "end_timestamp_ns", "local root span id", "trace endpoint",
    /// "span id"
    #[inline]
    pub fn is_well_known(offset: StringOffset) -> bool {
        u32::from(offset) < Self::WELL_KNOWN_COUNT as u32
    }

    // It varies by implementation, but frequently I've noticed that the
    // capacity after the first insertion is quite small, as in 3. This is a
    // bit too small and there are frequent reallocations. For one sample with
    // endpoint + code hotspots, we'd have at least these strings:
    // - ""
    // - At least one sample type
    // - At least one sample unit--already at 3 without any samples.
    // - "local root span id"
    // - "span id"
    // - "trace endpoint"
    // - A file and/or function name per frame.
    // So with a capacity like 3, we end up reallocating a bunch on or before
    // the very first sample. The number here is not fine-tuned, just skipping
    // some obviously bad, tiny sizes.
    const SET_INITIAL_CAPACITY: usize = 32;

    /// Creates a new string table with only the empty string.
    ///
    /// # Panics
    ///
    /// Panics if memory is unable to be allocated.
    pub fn new() -> Self {
        #[allow(clippy::unwrap_used)]
        Self::try_new().unwrap()
    }

    /// Tries to create a new string table with common well-known strings.
    ///
    /// # Errors
    ///
    /// Fails with [`StringTableError::OutOfMemory`] if the string table
    /// fails to allocate memory.
    pub fn try_new() -> Result<Self, StringTableError> {
        let bytes = ChainAllocator::new_in(Self::ARENA_SIZE_HINT, VirtualAllocator);
        let mut strings = HashSet::with_hasher(Hasher::default());

        if strings.try_reserve(Self::SET_INITIAL_CAPACITY).is_err() {
            return Err(Self::oom());
        }

        // Reserve space for well-known strings
        if strings.try_reserve(5).is_err() {
            return Err(Self::oom());
        }

        // Always hold the empty string as item 0. Do not insert it via intern
        // because that will try to allocate zero-bytes from the storage,
        // which is sketchy.
        strings.insert("");

        // These have stable positions too, making them well-known strings.
        strings.insert("end_timestamp_ns");
        strings.insert("local root span id");
        strings.insert("trace endpoint");
        strings.insert("span id");

        Ok(Self { bytes, strings })
    }

    /// Returns the number of strings currently held in the string table.
    #[inline]
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// Adds the string to the string table if it isn't present already, and
    /// returns a [`StringOffset`] that corresponds to the order that this
    /// string was originally inserted.
    ///
    /// # Panics
    ///
    /// Unwraps the call to [`Self::try_intern`]; see it for more details on
    /// when this can panic.
    pub fn intern(&mut self, str: &str) -> StringOffset {
        #[allow(clippy::unwrap_used)]
        self.try_intern(str).unwrap()
    }

    /// Tries to add the string to the string table if it isn't present, and
    /// returns a [`StringOffset`] that corresponds to the order that this
    /// string was originally inserted.
    ///
    /// # Errors
    ///
    ///  1. Returns [`StringTableError::OutOfMemory`] if memory needs to be allocated and it fails.
    ///  2. Returns [`StringTableError::StorageFull`] if the string offset overflows a `u32`.
    pub fn try_intern(&mut self, str: &str) -> Result<StringOffset, StringTableError> {
        let set = &mut self.strings;
        Ok(match set.get_index_of(str) {
            // SAFETY: it must fit, or it wouldn't exist in the table.
            Some(offset) => unsafe { StringOffset::try_from(offset).unwrap_unchecked() },
            None => {
                // No match. Get the current size of the table, which
                // corresponds to the StringId it will have when inserted.
                let Ok(string_id) = StringOffset::try_from(set.len()) else {
                    return Err(Self::full());
                };

                if self.strings.try_reserve(1).is_err() {
                    return Err(Self::oom());
                }

                // Make a new string in the arena, and fudge its lifetime
                // to appease the borrow checker.
                let new_str = {
                    // PANIC: the intern API doesn't allow for failure, so if
                    // this allocation fails, panic. The current
                    // implementation of `ChainAllocator` will fail if the
                    // underlying allocator fails when asking for a new chunk.
                    // This is expected to be rare.
                    #[allow(clippy::expect_used)]
                    let Ok(s) = self.bytes.allocate_str(str) else {
                        return Err(Self::oom());
                    };

                    // SAFETY: all references to this value get re-narrowed to
                    // the lifetime of the string table or iterator when
                    // exposed to the user. The string table and iterator will
                    // keep the arena alive, making the access safe.
                    unsafe { core::mem::transmute::<&str, &'static str>(s) }
                };

                // Add it to the set. We reserved memory for this earlier.
                self.strings.insert(new_str);

                string_id
            }
        })
    }

    /// Tries to find the string offset in the string table.
    ///
    /// # Errors
    ///
    /// Fails if the offset is not found.
    pub fn lookup(&self, offset: StringOffset) -> Result<&str, StringTableError> {
        self.strings
            .get_index(usize::from(offset))
            .copied()
            .ok_or_else(Self::not_found)
    }

    /// Clears the string table and reduces capacity back to the initial
    /// settings. The string table contains the empty string and common
    /// well-known strings after clearing.
    pub fn clear(&mut self) {
        // Dropping the arena allocator before reserving memory for the set
        // increases the likelihood that the try_reserve below will succeed.
        self.bytes = ChainAllocator::new_in(Self::ARENA_SIZE_HINT, VirtualAllocator);
        // Try to make a new hash set to reduce memory, as IndexSet does not
        // expose a fallible API for shrinking.
        let mut strings = HashSet::with_hasher(Hasher::default());
        if strings.try_reserve(Self::SET_INITIAL_CAPACITY).is_ok() {
            self.strings = strings;
        } else {
            // If making a new one fails, then use the old after clearing it.
            self.strings.clear();
        }

        // Always hold the empty string as item 0. Do not insert it via intern
        // because that will try to allocate zero-bytes from the storage,
        // which is sketchy.
        // This won't allocate memory because we've reserved it either in the
        // try_reserve above, or in the constructor.
        self.strings.insert("");

        // Re-add common endpoint-related strings
        let _ = self.try_intern("end_timestamp_ns");
        let _ = self.try_intern("local root span id");
        let _ = self.try_intern("trace endpoint");
    }

    /// Returns an iterator over the strings of the string table. The order
    /// of the iterator will match the order the strings were inserted.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.strings.iter().copied()
    }

    /// Tries to copy the provided strings from another string table.
    ///
    /// If this succeeds, then `to` will be fully initialized. On failure,
    /// the caller must assume that not all elements were initialized.
    ///
    /// # Errors
    ///
    ///  1. Returns [`StringTableError::OutOfMemory`] if `dst` fails to allocate.
    ///  2. Returns [`StringTableError::NotFound`] if any of the string offsets in `from` cannot be
    ///     found in `src`.
    ///  3. Returns [`StringTableError::StorageFull`] if a new string offset in `dst` wouldn't fit
    ///     in 32 bits.
    ///  4. Returns [`StringTableError::InvalidInput`] if the lengths of `to` and `from` do not
    ///     match.
    pub fn insert_from(
        &mut self,
        src: &StringTable,
        to: &mut [mem::MaybeUninit<StringOffset>],
        from: &[StringOffset],
    ) -> Result<(), StringTableError> {
        if to.len() != from.len() {
            return Err(Self::invalid_input());
        }
        for (from_off, to_off) in from.iter().zip(to) {
            let str = src.lookup(*from_off)?;
            let off = self.try_intern(str)?;
            to_off.write(off);
        }
        Ok(())
    }

    #[cold]
    fn invalid_input() -> StringTableError {
        StringTableError::InvalidInput
    }

    #[cold]
    fn not_found() -> StringTableError {
        StringTableError::NotFound
    }

    #[cold]
    fn oom() -> StringTableError {
        StringTableError::OutOfMemory
    }

    #[cold]
    fn full() -> StringTableError {
        StringTableError::StorageFull
    }
}

/// A [`LendingIterator`] for a [`StringTable`]. Make one by calling
/// [`StringTable::into_lending_iter`].
pub struct StringTableIter {
    /// This is actually used, the compiler doesn't know that the static
    /// references in `iter` actually point in here.
    #[allow(unused)]
    bytes: ChainAllocator<VirtualAllocator>,

    /// The strings of the string table, in order of insertion.
    /// The static lifetimes are a lie, they are tied to the `bytes`. When
    /// handing out references, bind the lifetime to the iterator's lifetime,
    /// which is a [`LendingIterator`] is needed.
    iter: <HashSet<&'static str> as IntoIterator>::IntoIter,
}

impl StringTableIter {
    fn new(string_table: StringTable) -> StringTableIter {
        StringTableIter {
            bytes: string_table.bytes,
            iter: string_table.strings.into_iter(),
        }
    }
}

impl LendingIterator for StringTableIter {
    type Item<'a>
        = &'a str
    where
        Self: 'a;

    fn next(&mut self) -> Option<Self::Item<'_>> {
        self.iter.next()
    }

    fn count(self) -> usize {
        self.iter.count()
    }
}

impl IntoLendingIterator for StringTable {
    type Iter = StringTableIter;

    fn into_lending_iter(self) -> Self::Iter {
        StringTableIter::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-known strings that should always be present in a new
    /// [`StringTable`], in order.
    const WELL_KNOWN_STRINGS: &[&str] = &[
        "",
        "end_timestamp_ns",
        "local root span id",
        "trace endpoint",
        "span id",
    ];

    #[test]
    fn fuzz_arena_allocator() {
        bolero::check!()
            .with_type::<(usize, Vec<String>)>()
            .for_each(|(size_hint, strings)| {
                // If the size_hint is insanely large, get allowed allocation
                // failures.  These are not interesting, so avoid them.
                if *size_hint > 4 * 1024 * 1024 * 1024 {
                    return;
                }
                let bytes = ChainAllocator::new_in(*size_hint, VirtualAllocator);
                let mut allocated_strings = vec![];
                for string in strings {
                    let s = bytes.allocate_str(string).expect("allocation to succeed");
                    assert_eq!(s, string);
                    allocated_strings.push(s);
                }
                assert_eq!(strings.len(), allocated_strings.len());
                strings
                    .iter()
                    .zip(allocated_strings.iter())
                    .for_each(|(s, t)| assert_eq!(s, t));
            });
    }

    /// This is a fuzz test for the allocation optimized `StringTable`.
    /// It checks both safety (lack of crashes / sanitizer failures),
    /// as well as functional correctness (the table should behave like an
    /// ordered set).
    /// Limitations:
    ///   - The crate used here to generate Strings internally has a default range for the length of
    ///     a string, (0..=64) We should experiment with longer strings to see what happens. https://github.com/camshaft/bolero/blob/f401669697ffcbe7f34cbfd09fd57b93d5df734c/lib/bolero-generator/src/alloc/mod.rs#L17
    ///   - Since iterating is destructive, can only check the string values once.
    ///
    /// `cargo +nightly bolero test
    /// collections::string_table::tests::fuzz_string_table -T 1min`
    #[test]
    fn fuzz_string_table() {
        bolero::check!()
            .with_type::<Vec<String>>()
            .for_each(|strings| {
                // Compare our optimized implementation against a "golden" version
                // from the standard library.
                let mut golden_list = WELL_KNOWN_STRINGS.to_vec();
                let mut golden_set: std::collections::HashSet<&str> =
                    std::collections::HashSet::from_iter(WELL_KNOWN_STRINGS.iter().copied());
                let mut st = StringTable::new();

                for string in strings {
                    assert_eq!(st.len(), golden_set.len());
                    if golden_set.insert(string) {
                        golden_list.push(string);
                    }

                    let str_id = st.intern(string);
                    // The str_id should refer to the id_th string interned
                    // on the list.  We can't look inside the `StringTable`
                    // in a non-desctrive way, but fortunately we have the
                    // `golden_list` to compare against.
                    assert_eq!(string, golden_list[usize::from(str_id)]);
                }
                assert_eq!(st.len(), golden_list.len());
                assert_eq!(st.len(), golden_set.len());

                // Check that the strings remain in order
                let mut it = st.into_lending_iter();
                let mut idx = 0;
                while let Some(s) = it.next() {
                    assert_eq!(s, golden_list[idx]);
                    idx += 1;
                }
            })
    }

    #[test]
    fn test_basics() {
        let mut table = StringTable::new();
        // The well-known strings should already be present.
        assert_eq!(StringTable::WELL_KNOWN_COUNT, table.len());
        assert_eq!(StringOffset::ZERO, table.intern(""));
        assert_eq!(table.lookup(StringOffset::ZERO).unwrap(), "");

        // Intern a string literal to ensure ?Sized works.
        let string = table.intern("datadog");
        assert_eq!(
            StringOffset::new(StringTable::WELL_KNOWN_COUNT as u32),
            string
        );
        assert_eq!(StringTable::WELL_KNOWN_COUNT + 1, table.len());
        assert_eq!(table.lookup(string).unwrap(), "datadog");
    }

    #[track_caller]
    fn test_from_src(src: &[&str]) {
        // Build the expected result: well-known strings first, then unique strings from src
        assert_eq!(
            WELL_KNOWN_STRINGS.len(),
            StringTable::WELL_KNOWN_COUNT,
            "Ensure these are in sync"
        );
        let mut expected_order = WELL_KNOWN_STRINGS.to_vec();
        let mut unique_count = WELL_KNOWN_STRINGS.len();

        for &string in src {
            if !expected_order.contains(&string) {
                expected_order.push(string);
                unique_count += 1;
            }
        }

        // Insert all the strings.
        let mut table = StringTable::new();
        for string in src {
            table.intern(string);
        }
        assert_eq!(unique_count, table.len());

        // Re-inserting doesn't change the size.
        for string in src {
            table.intern(string);
        }
        assert_eq!(unique_count, table.len());

        // Check that they are ordered correctly when iterating.
        let mut actual_iter = table.into_lending_iter();
        let mut expected_iter = expected_order.iter();
        while let (Some(expected), Some(actual)) = (expected_iter.next(), actual_iter.next()) {
            assert_eq!(*expected, actual);
        }

        // The iterators should be exhausted at this point.
        assert_eq!(None, expected_iter.next());
        assert_eq!(0, actual_iter.count());
    }

    #[test]
    fn test_small_set_of_strings() {
        let cases: &[_] = &[
            "",
            "local root span id",
            "span id",
            "trace endpoint",
            "samples",
            "count",
            "wall-time",
            "nanoseconds",
            "cpu-time",
            "<?php",
            "/srv/demo/public/index.php",
            "pid",
            "/var/www/public/index.php",
            "main",
            "thread id",
            "A\\Very\\Long\\Php\\Namespace\\Class::method",
            "/",
        ];
        test_from_src(cases);
    }

    /// Test inserting strings from a WordPress profile.
    /// Here we're checking that we don't panic or otherwise fail, and that
    /// the total number of strings and the bytes of those strings match.
    #[test]
    fn test_wordpress() {
        test_from_src(&wordpress_test_data::WORDPRESS_STRINGS);
    }
}
