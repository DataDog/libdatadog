// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The global heap as an [`Allocator`].

use super::{AllocError, Allocator};
use core::alloc::Layout;
use core::ptr::{self, NonNull};
use rust_alloc::alloc::{alloc, alloc_zeroed, dealloc, realloc};

/// The global memory allocator: whatever `#[global_allocator]` the program
/// registers, or the system allocator through std.
///
/// This mirrors `alloc::alloc::Global`, which is stable since Rust 1.100 and
/// can replace this type once the crate's minimum Rust version reaches it. Like
/// std's, it never calls `handle_alloc_error`: failures are returned as
/// [`AllocError`]. Zero-sized requests get a dangling, aligned pointer and are
/// never passed to the global allocator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Global;

/// An empty block at a dangling address equal to the alignment, like
/// `Layout::dangling_ptr`, which is newer than this crate's minimum Rust.
fn dangling(layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
    // The alignment is never zero, so this is never null.
    let ptr = NonNull::new(ptr::without_provenance_mut::<u8>(layout.align())).ok_or(AllocError)?;
    Ok(NonNull::slice_from_raw_parts(ptr, 0))
}

impl Global {
    #[inline]
    fn alloc_impl(layout: Layout, zeroed: bool) -> Result<NonNull<[u8]>, AllocError> {
        match layout.size() {
            0 => dangling(layout),
            size => {
                // SAFETY: `layout` has a nonzero size.
                let raw = unsafe {
                    if zeroed {
                        alloc_zeroed(layout)
                    } else {
                        alloc(layout)
                    }
                };
                let ptr = NonNull::new(raw).ok_or(AllocError)?;
                Ok(NonNull::slice_from_raw_parts(ptr, size))
            }
        }
    }

    /// # Safety
    ///
    /// As for [`Allocator::grow`].
    #[inline]
    unsafe fn grow_impl(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
        zeroed: bool,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        match old_layout.size() {
            0 => Self::alloc_impl(new_layout, zeroed),

            // SAFETY: `new_size` is nonzero because it is at least `old_size`.
            // The caller upholds the remaining conditions. `realloc` keeps the
            // first `old_size` bytes, and with `zeroed` the rest are zeroed.
            old_size if old_layout.align() == new_layout.align() => unsafe {
                let new_size = new_layout.size();
                let raw = realloc(ptr.as_ptr(), old_layout, new_size);
                let ptr = NonNull::new(raw).ok_or(AllocError)?;
                if zeroed {
                    raw.add(old_size).write_bytes(0, new_size - old_size);
                }
                Ok(NonNull::slice_from_raw_parts(ptr, new_size))
            },

            // SAFETY: both blocks are valid for `old_size` bytes and cannot
            // overlap, because the old one is still allocated. The caller
            // upholds the conditions for deallocating it.
            old_size => unsafe {
                let new_ptr = Self::alloc_impl(new_layout, zeroed)?;
                ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.cast::<u8>().as_ptr(), old_size);
                self.deallocate(ptr, old_layout);
                Ok(new_ptr)
            },
        }
    }
}

// SAFETY: blocks come from the global allocator, which keeps them valid until
// they are deallocated; `Global` has no state, so every instance is
// equivalent. No method unwinds, and zero-sized blocks are never passed to the
// global allocator. Layouts are always the ones the caller requested, so
// "fitting" means equal to the original layout.
unsafe impl Allocator for Global {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        Self::alloc_impl(layout, false)
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        Self::alloc_impl(layout, true)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            // SAFETY: the size is nonzero, so the block came from the global
            // allocator with this exact layout (see the impl's safety note).
            unsafe { dealloc(ptr.as_ptr(), layout) }
        }
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the caller upholds `grow`'s conditions.
        unsafe { self.grow_impl(ptr, old_layout, new_layout, false) }
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the caller upholds `grow_zeroed`'s conditions.
        unsafe { self.grow_impl(ptr, old_layout, new_layout, true) }
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() <= old_layout.size(),
            "`new_layout.size()` must be smaller than or equal to `old_layout.size()`"
        );

        match new_layout.size() {
            // SAFETY: the caller upholds the conditions for deallocating.
            0 => unsafe {
                self.deallocate(ptr, old_layout);
                dangling(new_layout)
            },

            // SAFETY: `new_size` is nonzero, and the caller upholds the rest.
            new_size if old_layout.align() == new_layout.align() => unsafe {
                let raw = realloc(ptr.as_ptr(), old_layout, new_size);
                let ptr = NonNull::new(raw).ok_or(AllocError)?;
                Ok(NonNull::slice_from_raw_parts(ptr, new_size))
            },

            // SAFETY: both blocks are valid for `new_size` bytes and cannot
            // overlap, because the old one is still allocated.
            new_size => unsafe {
                let new_ptr = self.allocate(new_layout)?;
                ptr::copy_nonoverlapping(ptr.as_ptr(), new_ptr.cast::<u8>().as_ptr(), new_size);
                self.deallocate(ptr, old_layout);
                Ok(new_ptr)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(block: NonNull<[u8]>) -> &'static mut [u8] {
        // SAFETY: test blocks are live for the duration of each check.
        unsafe { core::slice::from_raw_parts_mut(block.cast::<u8>().as_ptr(), block.len()) }
    }

    #[test]
    fn zero_sized_requests_are_dangling_and_aligned() {
        let layout = Layout::from_size_align(0, 64).unwrap();
        let block = Global.allocate(layout).unwrap();
        assert_eq!(block.len(), 0);
        assert_eq!(block.cast::<u8>().as_ptr().addr(), 64);
        // SAFETY: it was just allocated with this layout.
        unsafe { Global.deallocate(block.cast(), layout) };
    }

    #[test]
    fn allocate_zeroed_is_zero() {
        let layout = Layout::from_size_align(4096, 16).unwrap();
        let block = Global.allocate_zeroed(layout).unwrap();
        assert!(block.cast::<u8>().as_ptr().addr().is_multiple_of(16));
        assert!(bytes(block).iter().all(|&b| b == 0));
        // SAFETY: it was just allocated with this layout.
        unsafe { Global.deallocate(block.cast(), layout) };
    }

    #[test]
    fn grow_and_shrink_keep_contents() {
        for (old_align, new_align) in [(8, 8), (8, 64)] {
            let small = Layout::from_size_align(24, old_align).unwrap();
            let big = Layout::from_size_align(200, new_align).unwrap();
            let block = Global.allocate(small).unwrap();
            bytes(block)
                .iter_mut()
                .zip(1_u8..)
                .for_each(|(b, n)| *b = n);

            // SAFETY: each step passes the block from the previous step with
            // the layout it was allocated with.
            let block = unsafe { Global.grow_zeroed(block.cast(), small, big) }.unwrap();
            assert!(block.cast::<u8>().as_ptr().addr().is_multiple_of(new_align));
            let grown = bytes(block);
            assert!(grown[..24].iter().zip(1_u8..).all(|(&b, n)| b == n));
            assert!(grown[24..].iter().all(|&b| b == 0));

            let back = Layout::from_size_align(16, old_align).unwrap();
            // SAFETY: as above.
            let block = unsafe { Global.shrink(block.cast(), big, back) }.unwrap();
            assert!(bytes(block)[..16].iter().zip(1_u8..).all(|(&b, n)| b == n));

            let none = Layout::from_size_align(0, old_align).unwrap();
            // SAFETY: as above.
            let block = unsafe { Global.shrink(block.cast(), back, none) }.unwrap();
            assert_eq!(block.len(), 0);
            // SAFETY: zero-sized blocks are never passed to the global allocator.
            unsafe { Global.deallocate(block.cast(), none) };
        }
    }

    #[test]
    fn grow_from_zero_sized_allocates() {
        let none = Layout::from_size_align(0, 8).unwrap();
        let some = Layout::from_size_align(32, 8).unwrap();
        let block = Global.allocate(none).unwrap();
        // SAFETY: the block was allocated with `none`.
        let block = unsafe { Global.grow_zeroed(block.cast(), none, some) }.unwrap();
        assert!(bytes(block).iter().all(|&b| b == 0));
        // SAFETY: it was grown to `some`.
        unsafe { Global.deallocate(block.cast(), some) };
    }
}
