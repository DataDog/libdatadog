// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::{Buffer, LenEncodable};
use crate::u31::u31;
use crate::TryReserveError;
use allocator_api2::alloc::Global;
use datadog_alloc::buffer::{MayGrowOps, NoGrowOps};
use hashbrown::HashTable;
use rustc_hash::FxHasher;

impl LenEncodable for &str {
    fn encoded_len(&self) -> usize {
        self.len()
    }

    unsafe fn encode_raw<T: MayGrowOps<u8>>(&self, buffer: &mut Buffer<T>) -> ByteRange {
        let start = buffer.len_u31();
        buffer.extend_from_slice_within_capacity(self.as_bytes());
        let end = buffer.len_u31();
        ByteRange { start, end }
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StringOffset {
    pub(crate) offset: u32,
}

impl StringOffset {
    pub const ZERO: Self = Self { offset: 0 };

    /// # Safety
    /// The offset should exist in the string table. If it doesn't, then it
    /// shouldn't be looked up.
    pub const unsafe fn new_unchecked(offset: u32) -> Self {
        Self { offset }
    }
}

// This type exists because Range<u32> is not Copy.
#[derive(Clone, Copy, Debug)]
pub struct ByteRange {
    pub(crate) start: u31,
    pub(crate) end: u31,
}

#[derive(Clone, Debug)]
pub struct StringTable {
    ht: HashTable<(ByteRange, StringOffset), Global>,
}

#[derive(thiserror::Error, Debug)]
pub enum StringTableError {
    /// The requested string size is larger than the StringTable's limit.
    #[error("requested string is too large (`0` bytes)")]
    TooLarge(usize),

    #[error("string table failed: `0`")]
    TryReserveError(#[from] TryReserveError),
}

impl StringTable {
    /// This isn't required by protobuf, but rather a pragmatic idea that no
    /// individual string shouldn't be 64 KiB or larger--these are filenames,
    /// function names, labels, etc. Realistically, they should probably be
    /// <= 4 KiB, but there's no limit at all today, and we don't wish to
    /// cause too much pain at this time.
    pub const MAX_STR_LEN: usize = u16::MAX as usize;

    #[inline]
    fn project(buffer: &impl NoGrowOps<u8>, byte_range: ByteRange) -> &str {
        let range = core::ops::Range {
            start: byte_range.start.0 as usize,
            end: byte_range.end.0 as usize,
        };
        // SAFETY: the ByteRange's are not exposed, and we constructed them
        // in-range, and we never modify the existing bytes (only append).
        let bytes = unsafe { buffer.get_unchecked(range) };
        // SAFETY: we only inserted valid utf8 chars, and byte slices represent
        // complete strings (not sliced on different byte boundaries).
        unsafe { core::str::from_utf8_unchecked(bytes) }
    }

    #[inline]
    fn hash(str: &str) -> u64 {
        use core::hash::Hasher;

        let mut hasher = FxHasher::default();
        hasher.write(str.as_bytes());
        hasher.finish()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ht.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ht.is_empty()
    }

    #[inline]
    pub fn try_new<T: MayGrowOps<u8>>(buffer: &mut Buffer<T>) -> Result<Self, StringTableError> {
        let mut ht = HashTable::new();
        // Even very small profiles tend to have a certain number of strings,
        // because they need to describe the sample types, labels, function
        // and file names, etc.
        if let Err(err) = ht.try_reserve(32, |_| 0) {
            return Err(StringTableError::TryReserveError(match err {
                hashbrown::TryReserveError::CapacityOverflow => TryReserveError::CapacityOverflow,
                hashbrown::TryReserveError::AllocError { .. } => TryReserveError::AllocError,
            }));
        };
        let mut string_table = StringTable { ht };
        // The string table always has the empty string.
        string_table.try_add(buffer, "")?;
        Ok(string_table)
    }

    pub fn try_add<T: MayGrowOps<u8>>(
        &mut self,
        buffer: &mut Buffer<T>,
        str: &str,
    ) -> Result<StringOffset, StringTableError> {
        if str.len() > Self::MAX_STR_LEN {
            return Err(StringTableError::TooLarge(str.len()));
        }

        // First we hash the string and try to find it in the table.
        let hash = Self::hash(str);
        let eq = |(range, _off): &(ByteRange, StringOffset)| {
            let bytes2 = Self::project(buffer, *range);
            str == bytes2
        };
        // If we find it, we're done.
        if let Some((_range, off)) = self.ht.find(hash, eq) {
            return Ok(*off);
        }

        // Didn't find it, so serialize the string to the buffer.
        let checkpoint = buffer.len();
        let byte_range = super::try_encode_no_zero_size_opt(buffer, 6, &str)?;

        let hasher = |(byte_range, _off): &(ByteRange, StringOffset)| {
            let str = Self::project(buffer, *byte_range);
            Self::hash(str)
        };
        // To add the string, we need to reserve space for it.
        let len = self.ht.len();
        if let Err(err) = self.ht.try_reserve(1, hasher) {
            // truncate back the buffer, since it's not used anymore.
            buffer.truncate(checkpoint);
            return Err(StringTableError::TryReserveError(match err {
                hashbrown::TryReserveError::CapacityOverflow => TryReserveError::CapacityOverflow,
                hashbrown::TryReserveError::AllocError { .. } => TryReserveError::AllocError,
            }));
        }

        let offset = StringOffset { offset: len as u32 };

        // Finally, insert the string.
        _ = self.ht.insert_unique(hash, (byte_range, offset), hasher);
        Ok(offset)
    }
}
