// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::AllocError;
use crate::alloc::r#virtual::{Mapping, OsVirtualAllocator, VirtualAllocator};
use core::ptr::NonNull;
use std::alloc::Layout;
use std::cell::Cell;
use std::io;

#[derive(Debug)]
pub struct ArenaAllocator<A: VirtualAllocator = OsVirtualAllocator> {
    pub(crate) mapping: Option<Mapping<A>>,
    free_offset: Cell<usize>,
}

impl<A: VirtualAllocator> Default for ArenaAllocator<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl ArenaAllocator<OsVirtualAllocator> {
    pub fn with_capacity(capacity: usize) -> io::Result<ArenaAllocator<OsVirtualAllocator>> {
        Self::with_capacity_in(capacity, OsVirtualAllocator {})
    }
}

impl<A: VirtualAllocator> ArenaAllocator<A> {
    /// Creates a new arena allocator which has a capacity of zero. It will
    /// not request a virtual mapping from the OS.
    pub const fn new() -> Self {
        Self {
            mapping: None,
            free_offset: Cell::new(0),
        }
    }

    /// Creates an arena allocator whose underlying buffer holds at least
    /// `capacity` bytes. It will round up to a page size, except for capacity
    /// of zero.
    pub fn with_capacity_in(capacity: usize, alloc: A) -> io::Result<Self> {
        if capacity == 0 {
            return Ok(Self::new());
        }

        let mapping = Some(Mapping::new_in(capacity, alloc)?);

        let free_offset = Cell::new(0);
        Ok(Self {
            mapping,
            free_offset,
        })
    }

    pub fn remaining_capacity(&self) -> usize {
        match &self.mapping {
            None => 0,
            Some(mapping) => mapping.allocation_size() - self.free_offset.get(),
        }
    }

    /// Allocates the given layout. It will be zero-initialized. Allows for
    /// zero-sized allocations, which are not guaranteed to have unique
    /// addresses.
    pub fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let layout = layout.pad_to_align();
        let size = layout.size();
        if size == 0 {
            return Ok(NonNull::from(&mut []));
        }

        let mapping = self.mapping.as_ref().ok_or(AllocError)?;
        let base_ptr = mapping.base_in_bounds_ptr();
        let unaligned_ptr = base_ptr.add(self.free_offset.get())?;
        let aligned_ptr = unaligned_ptr.align_to(layout.align())?;
        let slice = aligned_ptr.slice(size)?;
        let free_offset = aligned_ptr.offset + size;
        self.free_offset.set(free_offset);
        Ok(slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc::page_size;

    #[test]
    fn test_capacity_0() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(0)?;

        // This should fail to allocate, arena no capacity cannot allocate.
        arena.allocate_zeroed(Layout::new::<u8>()).unwrap_err();
        Ok(())
    }

    /// Practically speaking, if this failes we've _actually_ invoked UB
    /// because the writes of zero are what considered this memory to be
    /// initialized in the first place.
    #[track_caller]
    fn check_zero(fatptr: NonNull<[u8]>) {
        let slice = unsafe { &*fatptr.as_ptr() };
        for i in slice {
            assert_eq!(0, *i);
        }
    }

    #[test]
    fn test_arena_basic_exhaustion() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(1)?;

        let expected_size = page_size();
        let actual_size = arena.remaining_capacity();
        assert_eq!(expected_size, actual_size);

        // This should consume the whole arena.
        let fatptr = arena.allocate_zeroed(Layout::from_size_align(expected_size, 1)?)?;
        check_zero(fatptr);

        // This should fail to allocate, zero bytes available.
        arena.allocate_zeroed(Layout::new::<u8>()).unwrap_err();

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

        let first = arena.allocate_zeroed(layout)?;
        let second = arena.allocate_zeroed(layout)?;
        let third = arena.allocate_zeroed(layout)?;
        let fourth = arena.allocate_zeroed(layout)?;

        check_zero(first);
        check_zero(second);
        check_zero(third);
        check_zero(fourth);

        // This _may_ fail to allocate, because we're only guaranteed 32 bytes
        // but in practice, it won't fail because it's rounded to a page size,
        // and I've never seen pages that small, even for 16 bit. However, in
        // any case, it should not panic, which is the point of the call.
        _ = std::hint::black_box(arena.allocate_zeroed(Layout::new::<u8>()));

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

        let first = arena.allocate_zeroed(layout)?;
        assert_eq!(DISTANCE / 2, first.len());
        let second = arena.allocate_zeroed(layout)?;
        assert_eq!(DISTANCE / 2, second.len());

        check_zero(first);
        check_zero(second);

        expect_distance(first, second, DISTANCE / 2);

        Ok(())
    }

    #[track_caller]
    fn check_alignment(fatpr: NonNull<[u8]>, align_to: usize) {
        let thinptr = fatpr.as_ptr() as *mut u8;

        // Implementations are allowed to return usize::MAX unconditionally.
        // In practice, I haven't encountered this and the value is sensible.
        let off = thinptr.align_offset(align_to);
        assert_eq!(0, off);
    }

    #[track_caller]
    fn check_complex_layout(arena: &ArenaAllocator, pointer: Layout) -> anyhow::Result<()> {
        let bool = Layout::from_size_align(1, 1)?;
        let sixteen_bit = Layout::from_size_align(2, 2)?;

        /* The layout should look like this for 64-bit:
        ├──────┤ first
               ├┤ second
                ├┤ padding
                 ├─┤ third
                   ├──┤ padding
                      ├──────┤ fourth
         */

        let first = arena.allocate_zeroed(pointer)?;
        let second = arena.allocate_zeroed(bool)?;
        let third = arena.allocate_zeroed(sixteen_bit)?;
        let fourth = arena.allocate_zeroed(pointer)?;

        check_zero(first);
        check_zero(second);
        check_zero(third);
        check_zero(fourth);

        check_alignment(first, pointer.align());
        check_alignment(second, bool.align());
        check_alignment(third, sixteen_bit.align());
        check_alignment(fourth, pointer.align());

        expect_distance(first, second, pointer.size());
        expect_distance(second, third, sixteen_bit.size());

        // Measuring between 2nd and 4th is stable on 32/64/128 bit ptrs,
        // because it varies by exactly the pointer length. Measuring between
        // third and fourth would be platform dependent.
        expect_distance(second, fourth, pointer.size());
        Ok(())
    }

    #[test]
    fn test_arena_complex_alignment() -> anyhow::Result<()> {
        let arena = ArenaAllocator::with_capacity(64)?;

        // Test different pointer sizes. Although we only target 64-bit at the
        // moment, there are still types like u32, u64, and u128 which are
        // the same from the allocator's perspective. This way we can check
        // realistic size and alignments.
        check_complex_layout(&arena, Layout::from_size_align(4, 4)?)?;
        check_complex_layout(&arena, Layout::from_size_align(8, 8)?)?;
        check_complex_layout(&arena, Layout::from_size_align(16, 16)?)?;

        Ok(())
    }

    #[test]
    fn test_alloc_failure() {
        #[derive(Debug)]
        struct FailingVirtualAllocator {}

        impl VirtualAllocator for FailingVirtualAllocator {
            fn virtual_alloc(&self, _size: usize) -> io::Result<NonNull<[u8]>> {
                Err(io::Error::from(io::ErrorKind::Other))
            }

            unsafe fn virtual_free(&self, _fatptr: NonNull<[u8]>) -> io::Result<()> {
                Err(io::Error::from(io::ErrorKind::Other))
            }
        }

        // Should still work, zero-size doesn't use the allocator.
        _ = ArenaAllocator::with_capacity_in(0, FailingVirtualAllocator {}).unwrap();

        _ = ArenaAllocator::with_capacity_in(64, FailingVirtualAllocator {}).unwrap_err();
    }
}
