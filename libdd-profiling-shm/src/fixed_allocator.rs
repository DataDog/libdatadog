// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A single-shot allocator backed by a caller-provided fixed memory region.
//!
//! Designed for use with `hashbrown::HashTable` in shared memory. It hands out
//! the entire region on the first `allocate` call and rejects all subsequent
//! allocations. `deallocate` is a no-op (the caller owns the underlying memory).

use allocator_api2::alloc::{AllocError, Allocator};
use core::alloc::Layout;
use core::cell::Cell;
use core::ptr::NonNull;

/// A single-shot allocator over a fixed memory region.
///
/// On the first call to [`allocate`](Allocator::allocate), it returns the
/// entire region (if the requested layout fits). All subsequent allocations
/// fail with [`AllocError`]. Deallocation is a no-op.
///
/// This is intended to back exactly one `hashbrown::HashTable` whose data
/// lives in a pre-allocated shared memory region.
pub struct FixedAllocator {
    base: NonNull<u8>,
    size: usize,
    allocated: Cell<bool>,
}

impl FixedAllocator {
    /// Creates a new `FixedAllocator` over the given memory region.
    ///
    /// # Safety
    /// - `base` must point to a valid, writable memory region of at least `size` bytes.
    /// - The region must remain valid for the lifetime of this allocator and any allocations made
    ///   from it.
    /// - The caller must not use the region for other purposes while this allocator (or any
    ///   allocation it produced) is alive.
    pub unsafe fn new(base: NonNull<u8>, size: usize) -> Self {
        Self {
            base,
            size,
            allocated: Cell::new(false),
        }
    }
}

unsafe impl Allocator for FixedAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // We only support a single allocation. hashbrown makes exactly one
        // allocation for its internal table.
        if self.allocated.get() {
            return Err(AllocError);
        }

        // Check alignment: the base must be aligned for the requested layout.
        let addr = self.base.as_ptr() as usize;
        let aligned_addr =
            addr.checked_add(layout.align() - 1).ok_or(AllocError)? & !(layout.align() - 1);
        let padding = aligned_addr - addr;

        let available = self.size.checked_sub(padding).ok_or(AllocError)?;
        if layout.size() > available {
            return Err(AllocError);
        }

        self.allocated.set(true);

        // Return exactly the requested size -- no excess.
        let ptr = unsafe { NonNull::new_unchecked(self.base.as_ptr().add(padding)) };
        Ok(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        // No-op: the caller owns the underlying memory (e.g., mmap region).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::alloc::Layout;

    #[test]
    fn allocate_succeeds_once() {
        let mut buf = vec![0u8; 4096];
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, 4096) };

        let layout = Layout::from_size_align(1024, 8).unwrap();
        let result = alloc.allocate(layout);
        assert!(result.is_ok());

        let ptr = result.unwrap();
        // Should return exactly the requested size.
        assert_eq!(ptr.len(), 1024);
    }

    #[test]
    fn second_allocate_fails() {
        let mut buf = vec![0u8; 4096];
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, 4096) };

        let layout = Layout::from_size_align(1024, 8).unwrap();
        assert!(alloc.allocate(layout).is_ok());
        assert!(alloc.allocate(layout).is_err());
    }

    #[test]
    fn too_large_allocation_fails() {
        let mut buf = vec![0u8; 64];
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, 64) };

        let layout = Layout::from_size_align(128, 1).unwrap();
        assert!(alloc.allocate(layout).is_err());
    }

    #[test]
    fn alignment_padding_accounted_for() {
        // Allocate a buffer that may not be aligned to 64 bytes.
        let mut buf = vec![0u8; 4096];
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, 4096) };

        let layout = Layout::from_size_align(32, 64).unwrap();
        let result = alloc.allocate(layout);
        assert!(result.is_ok());
        let ptr = result.unwrap();
        // The returned pointer must be aligned.
        assert_eq!(ptr.as_ptr() as *mut u8 as usize % 64, 0);
    }

    #[test]
    fn zero_size_region() {
        let base = NonNull::dangling();
        let alloc = unsafe { FixedAllocator::new(base, 0) };

        let layout = Layout::from_size_align(1, 1).unwrap();
        assert!(alloc.allocate(layout).is_err());
    }

    #[test]
    fn deallocate_is_noop() {
        let mut buf = vec![0u8; 4096];
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, 4096) };

        let layout = Layout::from_size_align(1024, 8).unwrap();
        let ptr = alloc.allocate(layout).unwrap();

        // Should not panic or crash.
        unsafe { alloc.deallocate(ptr.cast(), layout) };
    }

    #[test]
    fn works_with_hashbrown() {
        use hashbrown::HashTable;

        let mut buf = vec![0u8; 64 * 1024]; // 64 KiB
        let base = NonNull::new(buf.as_mut_ptr()).unwrap();
        let alloc = unsafe { FixedAllocator::new(base, buf.len()) };

        let mut table: HashTable<u32, FixedAllocator> = HashTable::new_in(alloc);

        // Pre-reserve capacity. hashbrown will call allocate once.
        table
            .try_reserve(100, |_| unreachable!())
            .expect("reserve should succeed");

        // Insert some entries.
        let hasher = |val: &u32| *val as u64;
        for i in 0u32..50 {
            table.insert_unique(hasher(&i), i, hasher);
        }

        // Verify lookups.
        for i in 0u32..50 {
            let found = table.find(hasher(&i), |v| *v == i);
            assert_eq!(found, Some(&i));
        }

        // Non-existent entry.
        assert!(table.find(hasher(&999), |v| *v == 999).is_none());
    }
}
