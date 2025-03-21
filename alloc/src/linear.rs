// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{AllocError, Allocator};
use core::alloc::Layout;
use core::cell::Cell;
use core::ptr::{slice_from_raw_parts_mut, NonNull};

/// [LinearAllocator] is an arena allocator, meaning that deallocating
/// individual allocations made by this allocator does nothing. Instead, the
/// whole backing memory is dropped at once. Destructors for these objects
/// are not called automatically and must be done by the caller if it's
/// necessary.
///
/// Once the slice of memory that underpins the LinearAllocator has been
/// allocated, allocations will begin to fail. It will not find new memory
/// to back allocations.
pub struct LinearAllocator<A: Allocator> {
    allocation_ptr: NonNull<u8>,
    allocation_layout: Layout,
    size: Cell<usize>,
    allocator: A,
}

unsafe impl<A: Allocator> Send for LinearAllocator<A> {}

impl<A: Allocator> LinearAllocator<A> {
    /// Creates a new [LinearAllocator] by requesting the `layout` from the
    /// provided `allocator`. Note that if the allocation is over-sized,
    /// meaning it's larger than the requested `layout.size()`, then the
    /// [LinearAllocator] will utilize this excess.
    pub fn new_in(layout: Layout, allocator: A) -> Result<Self, AllocError> {
        let allocation = allocator.allocate(layout)?;
        // SAFETY: this is the size/align of the actual allocation, so it must
        // be valid since the object exists.
        let allocation_layout =
            unsafe { Layout::from_size_align(allocation.len(), layout.align()).unwrap_unchecked() };
        Ok(Self {
            allocation_ptr: allocation.cast(),
            allocation_layout,
            size: Cell::new(0),
            allocator,
        })
    }

    /// Get the number of bytes allocated.
    #[inline]
    pub fn used_bytes(&self) -> usize {
        self.size.get()
    }

    /// Get the number of bytes allocated by the underlying allocator.
    /// This number is greater than or equal to [Self::used_bytes].
    #[inline]
    pub fn reserved_bytes(&self) -> usize {
        self.allocation_layout.size()
    }

    /// Gets the number of bytes that can be allocated without requesting more
    /// from the underlying allocator.
    pub fn remaining_capacity(&self) -> usize {
        self.reserved_bytes() - self.used_bytes()
    }

    fn base_ptr(&self) -> *mut u8 {
        self.allocation_ptr.as_ptr()
    }

    /// Determine if the given layout will fit in the current allocator
    pub fn has_capacity_for(&self, layout: Layout) -> bool {
        // SAFETY: base_ptr + size will always be in the allocated range, or
        // be the legally allowed one-past-the-end. If it doesn't fit, that's
        // a serious bug elsewhere in our logic.
        let align_offset =
            unsafe { self.base_ptr().add(self.used_bytes()) }.align_offset(layout.align());
        if let Some(needed_size) = align_offset.checked_add(layout.size()) {
            self.remaining_capacity() >= needed_size
        } else {
            false
        }
    }
}

impl<A: Allocator> Drop for LinearAllocator<A> {
    fn drop(&mut self) {
        let ptr = self.allocation_ptr;
        let layout = self.allocation_layout;
        // SAFETY: passing the original ptr back in, with a compatible layout.
        unsafe { self.allocator.deallocate(ptr, layout) };
    }
}

unsafe impl<A: Allocator> Allocator for LinearAllocator<A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 {
            return Err(AllocError);
        }

        // Find the needed allocation size including the necessary alignment.
        let size = self.used_bytes();
        // SAFETY: base_ptr + size will always be in the allocated range, or
        // be the legally allowed one-past-the-end. If it doesn't fit, that's
        // a serious bug elsewhere in our logic.
        let align_offset = unsafe { self.base_ptr().add(size) }.align_offset(layout.align());
        let needed_size = align_offset.checked_add(layout.size()).ok_or(AllocError)?;
        let remaining_capacity = self.reserved_bytes() - size;

        // Fail if there isn't room.
        if needed_size > remaining_capacity {
            return Err(AllocError);
        }

        // Create a wide pointer to the correct place and len.
        let wide_ptr = {
            // SAFETY: just checked above that base_ptr + align_offset + size
            // of the requested layout fits within the underlying allocation.
            let thin_ptr = unsafe { self.base_ptr().add(size + align_offset) };

            // Do a debug check that the pointer is actually aligned.
            debug_assert_eq!(0, thin_ptr.align_offset(layout.align()));
            slice_from_raw_parts_mut(thin_ptr, layout.size())
        };

        // SAFETY: derived from the underlying allocation pointer, so it is
        // inherently not null.
        let non_null = unsafe { NonNull::new_unchecked(wide_ptr) };

        // Update the size before returning.
        self.size.set(size + needed_size);
        Ok(non_null)
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        // This is an arena. It does batch de-allocation when dropped.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::*;
    use allocator_api2::alloc::Global;
    use bolero::generator::TypeGenerator;

    #[test]
    fn fuzz() {
        // avoid SUMMARY: libFuzzer: out-of-memory
        const MAX_SIZE: usize = 0x10000000;

        let size_hint = 0..=MAX_SIZE;
        let align_bits = 0..=32;
        let size = 0..=MAX_SIZE;
        let idx = 0..=MAX_SIZE;
        let val = u8::produce();
        let allocs = Vec::<(usize, u32, usize, u8)>::produce()
            .with()
            .values((size, align_bits, idx, val));
        bolero::check!()
            .with_generator((size_hint, allocs))
            .for_each(|(size_hint, size_align_vec)| {
                let allocator = LinearAllocator::new_in(
                    Layout::from_size_align(*size_hint, 1).unwrap(),
                    Global,
                )
                .unwrap();

                for (size, align_bits, idx, val) in size_align_vec {
                    fuzzer_inner_loop(&allocator, *size, *align_bits, *idx, *val, MAX_SIZE)
                }
            })
    }

    #[test]
    fn test_basics() -> Result<(), AllocError> {
        let alloc = LinearAllocator::new_in(Layout::array::<u8>(24).unwrap(), Global)?;
        const WIDTH: usize = 8;
        let layout = Layout::new::<[u8; WIDTH]>();
        assert!(alloc.has_capacity_for(layout));
        let first = alloc.allocate(layout)?;
        assert!(alloc.has_capacity_for(layout));
        let second = alloc.allocate(layout)?;
        assert!(alloc.has_capacity_for(layout));
        let third = alloc.allocate(layout)?;

        assert_ne!(first.as_ptr(), second.as_ptr());
        assert_ne!(first.as_ptr(), third.as_ptr());
        assert_ne!(second.as_ptr(), third.as_ptr());

        // LinearAllocator doesn't over-allocate, so we can test exact widths
        // and distances apart.
        assert_eq!(WIDTH, first.len());
        assert_eq!(WIDTH, second.len());
        assert_eq!(WIDTH, third.len());

        let first = first.as_ptr() as *mut u8;
        let second = second.as_ptr() as *mut u8;
        let third = third.as_ptr() as *mut u8;

        unsafe {
            assert_eq!(WIDTH, second.offset_from(first) as usize);
            assert_eq!(WIDTH, third.offset_from(second) as usize);
        }

        // No capacity left.
        assert!(!alloc.has_capacity_for(Layout::new::<bool>()));
        _ = alloc.allocate(Layout::new::<bool>()).unwrap_err();

        Ok(())
    }
}

#[cfg(test)]
mod alignment_tests {
    use super::*;
    use allocator_api2::alloc::Global;
    use core::mem::{align_of, size_of};
    use core::ops::RangeInclusive;

    // This is the order things will be allocated in.
    #[repr(C)]
    struct S {
        first: u8,
        second: u16,
        third: u32,
        fourth: u64,
    }

    struct TestAllocator {
        wide_ptr: NonNull<[u8]>,
        align_to: usize,
        allocated: Cell<bool>,
    }

    fn align_offset(ptr: *const u8, align_to: usize) -> usize {
        let uintptr = ptr as usize;
        let rem = uintptr % align_to;
        if rem == 0 {
            0
        } else {
            align_to - rem
        }
    }

    impl Drop for TestAllocator {
        fn drop(&mut self) {
            #[cfg(debug_assertions)]
            if self.allocated.get() {
                panic!("TestAllocator dropped while allocation was still held.");
            }

            let layout = unsafe { Layout::from_size_align_unchecked(self.wide_ptr.len(), 1) };
            unsafe { Global.deallocate(self.wide_ptr.cast(), layout) };
        }
    }

    impl TestAllocator {
        #[track_caller]
        fn new(align_to: usize) -> Self {
            assert!(align_to <= 8);

            // Leave room for full S even when mis-aligned.
            let size = 2 * align_of::<S>() + size_of::<S>();
            let layout = unsafe { Layout::from_size_align_unchecked(size, 1) };
            let orig = Global.allocate(layout).unwrap();

            Self {
                wide_ptr: orig,
                align_to,
                allocated: Cell::new(false),
            }
        }
    }

    unsafe impl Allocator for TestAllocator {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            assert!(layout.size() <= self.wide_ptr.len());
            assert_eq!(1, layout.align());
            if self.allocated.get() {
                Err(AllocError)
            } else {
                self.allocated.set(true);
                let unaligned = self.wide_ptr.as_ptr() as *mut u8;
                let offset = align_offset(unaligned, self.align_to);
                let aligned = unsafe { unaligned.add(offset) };
                let wide = slice_from_raw_parts_mut(aligned, self.wide_ptr.len() - offset);
                Ok(unsafe { NonNull::new_unchecked(wide) })
            }
        }

        #[track_caller]
        unsafe fn deallocate(&self, _ptr: NonNull<u8>, layout: Layout) {
            assert!(self.allocated.get());
            let unaligned = self.wide_ptr.as_ptr() as *mut u8;
            let offset = align_offset(unaligned, self.align_to);
            assert_eq!(self.wide_ptr.len() - offset, layout.size());
            assert_eq!(1, layout.align());
            self.allocated.set(false);
        }
    }

    #[track_caller]
    fn test_alignment(align_to: usize) {
        let layout_u8 = Layout::new::<u8>();
        let layout_u16 = Layout::new::<u16>();
        let layout_u32 = Layout::new::<u32>();
        let layout_u64 = Layout::new::<u64>();

        let max_size = size_of::<S>() + align_of::<S>() - 1;
        let test_alloc = TestAllocator::new(align_to);

        let alloc = {
            let layout = Layout::array::<u8>(max_size).unwrap();
            LinearAllocator::new_in(layout, test_alloc).unwrap()
        };

        // To test alignment, allocate smallest to largest.
        assert!(alloc.has_capacity_for(layout_u8));
        let ptr_u8 = alloc.allocate(layout_u8).unwrap();
        assert!(alloc.has_capacity_for(layout_u16));
        let ptr_u16 = alloc.allocate(layout_u16).unwrap();
        assert!(alloc.has_capacity_for(layout_u32));
        let ptr_u32 = alloc.allocate(layout_u32).unwrap();
        assert!(alloc.has_capacity_for(layout_u64));
        let ptr_u64 = alloc.allocate(layout_u64).unwrap();

        // LinearAllocator doesn't over-allocate, so we can test exact widths.
        assert_eq!(layout_u8.size(), ptr_u8.len());
        assert_eq!(layout_u16.size(), ptr_u16.len());
        assert_eq!(layout_u32.size(), ptr_u32.len());
        assert_eq!(layout_u64.size(), ptr_u64.len());

        let thinptr_u8 = ptr_u8.as_ptr() as *mut u8;
        let thinptr_u16 = ptr_u16.as_ptr() as *mut u8;
        let thinptr_u32 = ptr_u32.as_ptr() as *mut u8;
        let thinptr_u64 = ptr_u64.as_ptr() as *mut u8;

        #[track_caller]
        fn assert_distance_in(second: *mut u8, first: *mut u8, range: RangeInclusive<usize>) {
            // SAFETY: pointers are part of the same underlying allocation.
            let distance = unsafe { second.offset_from(first) };
            let udistance = usize::try_from(distance).unwrap();
            assert!(range.contains(&udistance));
        }

        // These are a little permissive, but exact alignment is checked below.
        assert_distance_in(thinptr_u16, thinptr_u8, 1..=2);
        assert_distance_in(thinptr_u32, thinptr_u16, 2..=4);
        assert_distance_in(thinptr_u64, thinptr_u32, 4..=8);

        assert_eq!(0, thinptr_u8.align_offset(layout_u8.align()));
        assert_eq!(0, thinptr_u16.align_offset(layout_u16.align()));
        assert_eq!(0, thinptr_u32.align_offset(layout_u32.align()));
        assert_eq!(0, thinptr_u64.align_offset(layout_u64.align()));

        // There _may_ be a little bit of space left, depends on if the
        // underlying allocator over-allocates. But it should not panic.
        let has_capacity = alloc.has_capacity_for(layout_u64);
        assert_eq!(has_capacity, alloc.allocate(layout_u64).is_ok())
    }
    #[test]
    fn test_alignment_1() {
        test_alignment(1);
    }

    #[test]
    fn test_alignment_2() {
        test_alignment(2);
    }

    #[test]
    fn test_alignment_3() {
        test_alignment(3);
    }

    #[test]
    fn test_alignment_4() {
        test_alignment(4);
    }

    #[test]
    fn test_alignment_5() {
        test_alignment(5);
    }

    #[test]
    fn test_alignment_6() {
        test_alignment(6);
    }

    #[test]
    fn test_alignment_7() {
        test_alignment(7);
    }

    #[test]
    fn test_alignment_8() {
        test_alignment(8);
    }
}
