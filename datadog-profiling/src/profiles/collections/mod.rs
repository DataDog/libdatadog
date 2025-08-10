// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod fam_ptr;
mod slice_table;
mod string_set;
mod string_table;
mod table;
mod thin_str;

pub use slice_table::*;
pub use string_table::*;
pub use table::*;
pub use thin_str::*;

use std::ptr::slice_from_raw_parts;

/// Represents a `*const [T]` but compressed to 32-bit by using an offset from
/// the base pointer of the data.
#[derive(Clone, Copy, Debug)]
struct CompressedPtrSlice {
    offset: u32,
    length: u32,
}

impl CompressedPtrSlice {
    const fn new(offset: u32, length: u32) -> CompressedPtrSlice {
        CompressedPtrSlice { offset, length }
    }

    /// # Safety
    ///
    /// The `base_ptr` needs to be the one this slice was created from.
    const unsafe fn add_to<T>(self, array_ptr: *const [T]) -> *const [T] {
        let offset = self.offset as usize;
        let len = self.length as usize;
        debug_assert!(offset < array_ptr.len());
        debug_assert!(offset + len <= array_ptr.len());
        let ptr = array_ptr.cast::<T>().add(offset);
        slice_from_raw_parts(ptr, len)
    }
}
