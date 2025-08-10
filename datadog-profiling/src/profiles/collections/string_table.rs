// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{CompressedPtrSlice, SliceTable};
use crate::profiles::ProfileError;
use parking_lot::RwLockReadGuard;
use std::num::TryFromIntError;

// But actually it's a 30-bit offset plus a 32-bit length with a 2-bit tag for
// which bucket it belongs to. This means there are 4 buckets. A new bucket
// is allocated when the previous bucket is full.

#[repr(C)]
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct StringOffset(u32); // 31-bit for Otel compatibility.

impl std::fmt::Display for StringOffset {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl StringOffset {
    pub const ZERO: Self = Self(0);

    /// # Safety
    ///
    /// The `offset` must be less than or equal to [`i32::MAX`].
    #[inline]
    pub const unsafe fn new_unchecked(offset: u32) -> Self {
        Self(offset)
    }
}

impl TryFrom<usize> for StringOffset {
    type Error = TryFromIntError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Ok(Self(i32::try_from(value)? as u32))
    }
}

impl TryFrom<u32> for StringOffset {
    type Error = TryFromIntError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(Self(i32::try_from(value)? as u32))
    }
}

impl TryFrom<i32> for StringOffset {
    type Error = TryFromIntError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Ok(Self(u32::try_from(value)?))
    }
}

impl From<StringOffset> for usize {
    fn from(value: StringOffset) -> Self {
        value.0 as usize
    }
}

// For Otel.
impl From<StringOffset> for i32 {
    fn from(value: StringOffset) -> Self {
        value.0 as i32
    }
}

// For pprof.
impl From<StringOffset> for i64 {
    fn from(value: StringOffset) -> Self {
        value.0 as i64
    }
}

/// A thread-safe string table that returns [`StringOffset`]s
pub struct StringTable {
    slice_table: SliceTable<u8>,
}

impl StringTable {
    /// Well-known string offsets for commonly used strings
    pub const EMPTY_STRING: StringOffset = StringOffset::ZERO;
    pub const END_TIMESTAMP_NS_OFFSET: StringOffset = unsafe { StringOffset::new_unchecked(1) };
    pub const LOCAL_ROOT_SPAN_ID_OFFSET: StringOffset = unsafe { StringOffset::new_unchecked(2) };
    pub const TRACE_ENDPOINT_OFFSET: StringOffset = unsafe { StringOffset::new_unchecked(3) };
    pub const SPAN_ID_OFFSET: StringOffset = unsafe { StringOffset::new_unchecked(4) };

    /// Number of well-known strings: "", "end_timestamp_ns", "local root span id", "trace
    /// endpoint", "span id"
    pub const WELL_KNOWN_COUNT: usize = 5;

    /// Returns true if the given offset refers to a well-known string.
    /// Well-known strings are: "", "end_timestamp_ns", "local root span id", "trace endpoint",
    /// "span id"
    #[inline]
    pub const fn is_well_known(offset: StringOffset) -> bool {
        offset.0 < Self::WELL_KNOWN_COUNT as u32
    }
    pub fn try_new(capacity: usize, initial_ht_size: usize) -> Result<Self, ProfileError> {
        let slice_table =
            SliceTable::try_new(capacity, initial_ht_size.min(Self::WELL_KNOWN_COUNT))?;
        let string_table = Self { slice_table };
        // Currently I don't think these can fail. The collections will have
        // enough memory for the well-known strings due to the `min` above.
        // Although there's other data at the start of the memory page, there
        // should still be enough room for all these strings in practice (page
        // sizes are  typically 4-16 KiB).
        // I'm not aware of a clean way to instruct the optimizer to assume
        // this for code layout.
        string_table.try_intern("")?;
        string_table.try_intern("end_timestamp_ns")?;
        string_table.try_intern("local root span id")?;
        string_table.try_intern("trace endpoint")?;
        string_table.try_intern("span id")?;
        Ok(string_table)
    }

    pub fn try_intern(&self, value: &str) -> Result<StringOffset, ProfileError> {
        let id = self.slice_table.insert(value.as_bytes())?;
        // SAFETY: ProfileId's are also 31-bit for Otel compatibility.
        Ok(unsafe { StringOffset::new_unchecked(id.into_u32()) })
    }

    pub fn as_slice(&self) -> StringTableSlice {
        let base_ptr = self.slice_table.array_ptr();
        let vec = self.slice_table.acquire_read_lock();
        StringTableSlice { base_ptr, vec }
    }

    pub fn try_clone(&self) -> Result<Self, ProfileError> {
        let slice_table = self.slice_table.try_clone()?;
        Ok(Self { slice_table })
    }
}

pub struct StringTableSlice<'a> {
    base_ptr: *mut [u8],
    vec: RwLockReadGuard<'a, Vec<CompressedPtrSlice>>,
}

impl<'a> StringTableSlice<'a> {
    pub fn get(&self, id: StringOffset) -> Option<&str> {
        let compressed_pointer_slice = self.vec.get(usize::from(id))?;
        let bytes = unsafe { &*compressed_pointer_slice.add_to(self.base_ptr) };
        Some(unsafe { std::str::from_utf8_unchecked(bytes) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_concurrent_string_table() {
        // one page on Apple Silicon, 4 pages on most other platforms.
        let cap = 16 * 1024;
        let string_table = StringTable::try_new(cap, 12).unwrap();
        let cloned_table = string_table.try_clone().unwrap();
    }
}
