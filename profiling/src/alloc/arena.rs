// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::AllocError;
use crate::alloc::r#virtual::{alloc as virtual_alloc, Mapping};
use core::ptr;
use core::ptr::NonNull;
use std::alloc::Layout;
use std::cell::Cell;

pub struct ArenaAllocator {
    pub(crate) mapping: Option<Mapping>,
    remaining_capacity: Cell<usize>,
}

impl Default for ArenaAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl ArenaAllocator {
    pub const fn new() -> Self {
        Self {
            mapping: None,
            remaining_capacity: Cell::new(0),
        }
    }

    unsafe fn from_mapping(mapping: Mapping) -> Self {
        let remaining_capacity = Cell::new(mapping.len());
        Self {
            mapping: Some(mapping),
            remaining_capacity,
        }
    }

    /// Creates an arena allocator whose underlying buffer holds at least
    /// `capacity` bytes. It will round up to a page size, except for capacity
    /// of zero.
    pub fn with_capacity(capacity: usize) -> anyhow::Result<Self> {
        if capacity == 0 {
            return Ok(Self::new());
        }

        let region = virtual_alloc(capacity)?;

        // SAFETY: we haven't done any unsafe things with the region like give
        // out pointers to its interior bytes.
        Ok(unsafe { Self::from_mapping(region) })
    }

    pub fn remaining_capacity(&self) -> usize {
        self.remaining_capacity.get()
    }

    #[inline]
    pub fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate_zeroed(layout)
    }

    pub fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let layout = layout.pad_to_align();
        let size = layout.size();
        if size == 0 {
            // SAFETY: empty slice made from dangling non-null ptr is safe.
            let slice = ptr::slice_from_raw_parts_mut(NonNull::dangling().as_ptr(), 0);
            // SAFETY: the dangling ptr is by definition non-null.
            return Ok(unsafe { NonNull::new_unchecked(slice) });
        }

        let mapping = match self.mapping.as_ref() {
            None => return Err(AllocError),
            Some(m) => m,
        };

        let mut remaining_capacity = self.remaining_capacity.get();

        let base_ptr = mapping.base_non_null_ptr::<u8>().as_ptr();
        // SAFETY: todo
        let alloc_ptr = unsafe { base_ptr.add(mapping.len() - remaining_capacity) };

        // The alloc_ptr points to the first unallocated byte. The alignment
        // of the object to be allocated needs to be considered for both the
        // start of the object but also the remaining capacity.
        let align_offset = alloc_ptr.align_offset(layout.align());
        let needed_capacity = align_offset + layout.size();

        if needed_capacity > remaining_capacity {
            return Err(AllocError);
        }

        remaining_capacity -= needed_capacity;
        self.remaining_capacity.set(remaining_capacity);

        // SAFETY: the allocation has already been determined to fit in the
        // region, so the addition will fit within the region, and will also
        // not be null.
        let ptr = unsafe {
            let alloc_ptr = alloc_ptr.add(align_offset);
            let slice = ptr::slice_from_raw_parts_mut(alloc_ptr, size);
            NonNull::new_unchecked(slice)
        };
        Ok(ptr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::page_size;
    use std::mem;

    #[test]
    fn test_capacity_0() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(0)?;

        // This should fail to allocate, arena no capacity cannot allocate.
        arena.allocate(Layout::new::<u8>()).unwrap_err();
        Ok(())
    }

    #[test]
    fn test_arena_basic_exhaustion() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(1)?;

        let expected_size = page_size();
        let actual_size = arena.remaining_capacity();
        assert_eq!(expected_size, actual_size);

        // This should consume the whole arena.
        arena.allocate(Layout::from_size_align(expected_size, 1)?)?;

        // This should fail to allocate, zero bytes available.
        arena.allocate(Layout::new::<u8>()).unwrap_err();

        Ok(())
    }

    #[track_caller]
    fn expect_distance(first: NonNull<[u8]>, second: NonNull<[u8]>, distance: usize) {
        let a = first.as_ptr() as *mut u8;
        let b = second.as_ptr() as *mut u8;

        assert_eq!(b, unsafe { a.add(distance) });
    }

    #[test]
    fn test_arena_basics() -> anyhow::Result<()> {
        const DISTANCE: usize = 8;
        let arena = ArenaAllocator::with_capacity(DISTANCE * 4)?;

        // Four of these should fit.
        let layout = Layout::from_size_align(DISTANCE, DISTANCE)?;

        let first = arena.allocate(layout)?;
        let second = arena.allocate(layout)?;
        let third = arena.allocate(layout)?;
        let fourth = arena.allocate(layout)?;

        // This _may_ fail to allocate, because we're only guaranteed 32 bytes
        // but in practice, it won't fail because it's rounded to a page size,
        // and I've never seen pages that small, even for 16 bit. However, in
        // any case, it should not panic, which is the point of the call.
        _ = std::hint::black_box(arena.allocate(Layout::new::<u8>()));

        expect_distance(first, second, DISTANCE);
        expect_distance(second, third, DISTANCE);
        expect_distance(third, fourth, DISTANCE);

        Ok(())
    }

    #[test]
    fn test_arena_simple_alignment() -> anyhow::Result<()> {
        const DISTANCE: usize = 16;
        let arena = ArenaAllocator::with_capacity(DISTANCE)?;

        let layout = Layout::from_size_align(DISTANCE / 2, DISTANCE / 2)?;

        let first = arena.allocate(layout)?;
        assert_eq!(DISTANCE / 2, first.len());
        let second = arena.allocate(layout)?;
        assert_eq!(DISTANCE / 2, second.len());

        expect_distance(first, second, DISTANCE / 2);

        Ok(())
    }

    #[test]
    fn test_arena_complex_alignment() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(64)?;

        let pointer = Layout::new::<*const ()>();
        let bool = Layout::new::<bool>();

        let first = arena.allocate(pointer)?;
        let second = arena.allocate(bool)?;
        // third could be mis-aligned if alignment isn't considered.
        let third = arena.allocate(pointer)?;

        expect_distance(first, second, mem::size_of::<*const ()>());
        expect_distance(second, third, mem::size_of::<*const ()>());

        Ok(())
    }
}
