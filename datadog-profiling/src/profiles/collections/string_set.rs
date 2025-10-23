// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::slice_set::SliceSet;
use super::SetError;
use super::ThinStr;
use core::hash;
use std::ffi::c_void;
use std::hash::BuildHasher;
use std::ops::Deref;
use std::ptr::NonNull;

type Hasher = hash::BuildHasherDefault<rustc_hash::FxHasher>;

/// Represents a handle to a string that can be retrieved by the string set.
/// The exact representation is not a public detail; it is only available so
/// that it is known for FFI size and alignment.
///
/// Some [`StringRef`]s refer to well-known strings, which always exist in
/// every string table.
///
/// The caller needs to ensure the string set it was created from always exists
/// when a StringId is dereferenced.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringRef(pub ThinStr<'static>);

impl StringRef {
    pub fn into_raw(self) -> NonNull<c_void> {
        self.0.into_raw()
    }

    /// Re-creates a [`StringRef`] created by [`StringRef::into_raw`].
    ///
    /// # Safety
    ///
    /// `this` needs to be created from [``StringRef::into_raw`] and the set
    /// it belongs to should still be alive.
    pub unsafe fn from_raw(this: NonNull<c_void>) -> Self {
        Self(ThinStr::from_raw(this))
    }
}

impl From<&StringRef> for StringRef {
    fn from(value: &StringRef) -> Self {
        *value
    }
}

impl Default for StringRef {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl StringRef {
    pub const EMPTY: StringRef = StringRef(ThinStr::new());
    pub const END_TIMESTAMP_NS: StringRef = StringRef(ThinStr::end_timestamp_ns());
    pub const LOCAL_ROOT_SPAN_ID: StringRef = StringRef(ThinStr::local_root_span_id());
    pub const TRACE_ENDPOINT: StringRef = StringRef(ThinStr::trace_endpoint());
    pub const SPAN_ID: StringRef = StringRef(ThinStr::span_id());
}

// Safe conversion from FFI-facing StringId2 to internal StringRef.
// Maps the null/empty StringId2 to the non-null well-known EMPTY StringRef.
impl From<crate::api2::StringId2> for StringRef {
    fn from(id: crate::api2::StringId2) -> Self {
        if id.is_empty() {
            StringRef::EMPTY
        } else {
            // SAFETY: Non-empty StringId2 values originate from this string set and
            // carry a valid pointer to a length-prefixed ThinStr in our storage.
            unsafe { core::mem::transmute::<crate::api2::StringId2, StringRef>(id) }
        }
    }
}

/// Holds unique strings and provides [`StringRef`]s to fetch them later.
/// This is a newtype around SliceSet<u8> to enforce UTF-8 invariants.
pub struct UnsyncStringSet(SliceSet<u8>);

pub const WELL_KNOWN_STRING_IDS: [StringRef; 5] = [
    StringRef::EMPTY,
    StringRef::END_TIMESTAMP_NS,
    StringRef::LOCAL_ROOT_SPAN_ID,
    StringRef::TRACE_ENDPOINT,
    StringRef::SPAN_ID,
];

impl UnsyncStringSet {
    pub fn try_with_capacity(capacity: usize) -> Result<Self, SetError> {
        let mut set = Self(SliceSet::try_with_capacity(capacity)?);
        let strings = &mut set.0.slices;
        for id in WELL_KNOWN_STRING_IDS {
            let hash = Hasher::default().hash_one(id.0.deref().as_bytes());
            strings.insert_unique(hash, id.0.into(), |t| Hasher::default().hash_one(t.deref()));
        }

        Ok(set)
    }

    /// Creates a new string set, which initially holds the empty string and
    /// other well-known strings. The well-known strings are always
    /// available and can be fetched using the [`WELL_KNOWN_STRING_IDS`].
    pub fn try_new() -> Result<Self, SetError> {
        Self::try_with_capacity(28)
    }

    unsafe fn find_with_hash(&self, hash: u64, str: &str) -> Option<StringRef> {
        let interned_str = self.0.slices.find(hash, |thin_slice| {
            // SAFETY: We only store valid UTF-8 in string sets
            let slice_str = unsafe { std::str::from_utf8_unchecked(thin_slice.as_slice()) };
            slice_str == str
        })?;
        Some(StringRef((*interned_str).into()))
    }

    /// # Safety
    ///  1. The hash must be the same as if the str was re-hashed with the hasher the string set
    ///     would use.
    ///  2. The string must be unique within the set.
    pub unsafe fn insert_unique_uncontended(&mut self, str: &str) -> Result<StringRef, SetError> {
        let hash = Hasher::default().hash_one(str.as_bytes());
        self.insert_unique_uncontended_with_hash(hash, str)
    }

    /// Inserts a string into the string set without checking for duplicates, using a pre-calculated
    /// hash.
    ///
    /// # Safety
    ///  1. The caller must ensure that the hash was computed using the same hasher the string set
    ///     would use.
    ///  2. The string must be unique within the set.
    pub unsafe fn insert_unique_uncontended_with_hash(
        &mut self,
        hash: u64,
        str: &str,
    ) -> Result<StringRef, SetError> {
        let new_slice = self
            .0
            .insert_unique_uncontended_with_hash(hash, str.as_bytes())?;
        Ok(StringRef(new_slice.into()))
    }

    /// Adds the string to the string set if it isn't present already, and
    /// returns a handle to the string that can be used to retrieve it later.
    pub fn try_insert(&mut self, str: &str) -> Result<StringRef, SetError> {
        let hash = Hasher::default().hash_one(str.as_bytes());
        unsafe { self.try_insert_with_hash(hash, str) }
    }

    /// Adds the string to the string set if it isn't present already, using a pre-calculated hash.
    /// Returns a handle to the string that can be used to retrieve it later.
    ///
    /// # Safety
    /// The caller must ensure that the hash was computed using the same hasher the string set would
    /// use.
    pub unsafe fn try_insert_with_hash(
        &mut self,
        hash: u64,
        str: &str,
    ) -> Result<StringRef, SetError> {
        // SAFETY: the string's hash is correct, we use the same hasher as
        // StringSet uses.
        if let Some(id) = self.find_with_hash(hash, str) {
            return Ok(id);
        }

        // SAFETY: we just checked above that the string isn't in the set.
        self.insert_unique_uncontended_with_hash(hash, str)
    }

    /// Returns an iterator over all strings in the set as [`StringRef`]s.
    pub fn string_ids(&self) -> impl Iterator<Item = StringRef> + '_ {
        self.0.slices.iter().map(|slice| StringRef((*slice).into()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    /// # Safety
    /// The caller must ensure that the `StringId` was obtained from this set
    /// (or is a well-known id) and that the set outlives the returned `&str`.
    pub unsafe fn get_string(&self, id: StringRef) -> &str {
        // SAFETY: safe as long as caller respects this function's safety.
        unsafe { core::mem::transmute::<&str, &str>(id.0.deref()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_set_basic_operations() {
        let mut set = UnsyncStringSet::try_new().unwrap();

        // Test inserting new strings
        let id1 = set.try_insert("hello").unwrap();
        let id2 = set.try_insert("world").unwrap();
        let id3 = set.try_insert("hello").unwrap(); // duplicate

        // Verify duplicate returns same ID
        assert_eq!(&*id1.0, &*id3.0);
        assert_ne!(&*id1.0, &*id2.0);

        // Verify retrieval
        unsafe {
            assert_eq!(set.get_string(id1), "hello");
            assert_eq!(set.get_string(id2), "world");
            assert_eq!(set.get_string(id3), "hello");
        }
    }

    #[test]
    fn test_string_lengths_and_alignment() {
        let mut set = UnsyncStringSet::try_new().unwrap();

        // Test various string lengths that might cause alignment issues
        let test_strings = [
            "",                                    // 0 bytes
            "a",                                   // 1 byte
            "ab",                                  // 2 bytes
            "abc",                                 // 3 bytes
            "abcd",                                // 4 bytes
            "abcdefg",                             // 7 bytes
            "abcdefgh",                            // 8 bytes (usize boundary on 64-bit)
            "abcdefghijklmno",                     // 15 bytes
            "abcdefghijklmnop",                    // 16 bytes
            "abcdefghijklmnopqrstuvwxyz123456789", // 35 bytes
        ];

        let mut ids = Vec::new();
        for s in &test_strings {
            let id = set.try_insert(s).unwrap();
            ids.push(id);
        }

        // Verify all strings can be retrieved correctly
        for (id, expected) in ids.iter().zip(&test_strings) {
            unsafe {
                assert_eq!(set.get_string(*id), *expected);
            }
        }
    }

    #[test]
    fn test_unicode_strings() {
        let mut set = UnsyncStringSet::try_new().unwrap();

        let unicode_strings = [
            "café",         // Latin with accents
            "🦀",           // Emoji (4 bytes)
            "こんにちは",   // Japanese
            "Здравствуй",   // Cyrillic
            "🔥💯✨",       // Multiple emoji
            "a\u{0000}b",   // Embedded null
            "line1\nline2", // Newline
            "tab\there",    // Tab
        ];

        let mut ids = Vec::new();
        for s in &unicode_strings {
            let id = set.try_insert(s).unwrap();
            ids.push(id);
        }

        // Verify all Unicode strings are preserved correctly
        for (id, expected) in ids.iter().zip(&unicode_strings) {
            unsafe {
                assert_eq!(set.get_string(*id), *expected);
            }
        }
    }

    #[test]
    fn test_capacity_and_growth() {
        // Test with minimal capacity
        let mut set = UnsyncStringSet::try_with_capacity(1).unwrap();

        // Insert more strings than initial capacity to force growth
        let test_strings: Vec<String> = (0..50).map(|i| format!("growth_test_{}", i)).collect();

        let mut ids = Vec::new();
        for s in &test_strings {
            let id = set.try_insert(s).unwrap();
            ids.push(id);
        }

        // Verify all strings are still accessible after growth
        for (id, expected) in ids.iter().zip(&test_strings) {
            unsafe {
                assert_eq!(set.get_string(*id), expected);
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_large_strings() {
        let mut set = UnsyncStringSet::try_new().unwrap();

        // Test moderately large string
        let large_string = "x".repeat(1024);
        let id1 = set.try_insert(&large_string).unwrap();

        unsafe {
            assert_eq!(set.get_string(id1), large_string);
        }

        // Test very large string
        let very_large_string = "y".repeat(65536);
        let id2 = set.try_insert(&very_large_string).unwrap();

        unsafe {
            assert_eq!(set.get_string(id2), very_large_string);
            // Verify first string is still intact
            assert_eq!(set.get_string(id1), large_string);
        }

        // Test extremely large string (>2 MiB) to trigger different ChainAllocator path
        let huge_string = "z".repeat(2 * 1024 * 1024 + 1000); // >2 MiB
        let id3 = set.try_insert(&huge_string).unwrap();

        unsafe {
            assert_eq!(set.get_string(id3), huge_string);
            // Verify previous strings are still intact
            assert_eq!(set.get_string(id1), large_string);
            assert_eq!(set.get_string(id2), very_large_string);
        }
    }

    #[test]
    fn test_many_small_strings() {
        const NUM_STRINGS: usize = if cfg!(miri) { 100 } else { 1000 };
        let mut set = UnsyncStringSet::try_new().unwrap();

        // Insert many small strings to test fragmentation and growth
        let mut ids = Vec::with_capacity(NUM_STRINGS);
        let mut expected = Vec::with_capacity(NUM_STRINGS);

        for i in 0..NUM_STRINGS {
            let s = format!("{}", i);
            let id = set.try_insert(&s).unwrap();
            ids.push(id);
            expected.push(s);
        }

        // Verify all strings are still correct
        for (id, expected_str) in ids.iter().zip(&expected) {
            unsafe {
                assert_eq!(set.get_string(*id), expected_str);
            }
        }
    }
}
