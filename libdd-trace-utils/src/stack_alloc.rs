// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A bump allocator over inline (stack-resident) storage, implementing the
//! `allocator_api_2` [`Allocator`] trait.
//!
//! [`StackBump`] exists so that short-lived, single-scope scratch collections can be allocated out
//! of  a buffer living on the caller's stack, avoiding any heap traffic for the common
//! small-map case.
//!
//! Requests the inline buffer can't serve transparently fall back to the global (heap) allocator,
//! and such blocks are freed on deallocation. An under-sized buffer therefore only costs
//! performance, never correctness.

use allocator_api2::alloc::{AllocError, Allocator, Global};
use core::alloc::Layout;
use core::cell::{Cell, UnsafeCell};
use core::mem::MaybeUninit;
use core::ptr::NonNull;

/// Inline storage. The buffer is minimally 8-aligned so we
/// don't loose capacity when allocating pointers for instance.
/// from an initally non aligned adress.
#[repr(align(8))]
struct AlignedStorage<const N: usize>([MaybeUninit<u8>; N]);

/// A bump allocator handing out sub-ranges of an inline `N`-byte buffer.
///
/// Intended usage: declare a [`StackBump`] as a local (so the buffer lives on the
/// stack), then pass `&bump` to allocator-aware collections.
///
/// # Semantics
///
/// - Allocation is a pointer bump: bytes are handed out sequentially and never reused within the
///   lifetime of the allocator. This is only suitable for build-then-drop scratch usage, not for
///   collections that repeatedly allocate and deallocate.
/// - Individual deallocation of inline blocks is a no-op: the whole buffer is reclaimed at once
///   when the [`StackBump`] value goes out of scope. **Destructors of objects stored in handed-out
///   memory are not run automatically**, so only store values with no `Drop` side effects (or drop
///   them explicitly beforehand).
/// - Once the inline buffer is exhausted allocations transparently fall back to the global (heap)
///   allocator, and such blocks are freed on deallocation. An under-sized buffer therefore degrades
///   performance, never correctness.
pub(crate) struct StackBump<const N: usize> {
    /// Inline storage. `UnsafeCell` provides the interior mutability required to hand
    /// out mutable raw views of sub-ranges of the buffer through `&self`.
    storage: UnsafeCell<AlignedStorage<N>>,
    /// Number of bytes handed out so far. Invariant: `cursor <= N`.
    cursor: Cell<usize>,
}

impl<const N: usize> StackBump<N> {
    pub(crate) const fn new() -> Self {
        Self {
            storage: UnsafeCell::new(AlignedStorage([MaybeUninit::uninit(); N])),
            cursor: Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.cursor.get()
    }

    #[cfg(test)]
    pub(crate) fn remaining_bytes(&self) -> usize {
        N - self.cursor.get()
    }

    #[inline]
    fn base(&self) -> *mut u8 {
        self.storage.get().cast::<u8>()
    }

    /// Whether `ptr` points inside the inline buffer, i.e. the block was granted from
    /// it. The check uses the block's extent (`addr + size <= base + N`) rather than its
    /// start address alone, so zero-sized blocks granted at the boundary address
    /// `base + N` — one past the end, a valid zero-sized block — are recognized too.
    /// `layout` must match the one the block was allocated with, as the `Allocator`
    /// contract guarantees for `deallocate`.
    #[inline]
    fn is_inline(&self, ptr: NonNull<u8>, layout: Layout) -> bool {
        let addr = ptr.as_ptr() as usize;
        let end = addr.wrapping_add(layout.size());
        let base = self.base() as usize;
        addr >= base && end <= base.wrapping_add(N)
    }

    /// Bump-allocate `layout` out of the inline buffer, or `None` if it doesn't fit.
    fn allocate_inline(&self, layout: Layout) -> Option<NonNull<[u8]>> {
        let align = layout.align();

        // Pick the next start with the correct alignement for the layout
        // The Layout type guarantees that align is a power of two,
        let base = self.base() as usize;
        let pad = base.wrapping_add(self.cursor.get()).wrapping_neg() & (align - 1);
        let start = self.cursor.get().checked_add(pad)?;
        let end = start.checked_add(layout.size())?;
        if end > N {
            return None;
        }
        self.cursor.set(end);

        // SAFETY: `start + layout.size() == end <= N`, so the returned pointer and the
        // `layout.size()` bytes behind it are within the `N`-byte buffer owned by
        // `self`, which outlives every block it hands out (deallocation of inline
        // blocks is a no-op). `start == N` only happens for zero-sized requests, where
        // `base + N` is the one-past-the-end address: valid to form and to hand out
        // for a zero-sized block, and recognized as inline by `is_inline`.
        let ptr = unsafe { NonNull::new_unchecked(self.base().add(start)) };
        Some(NonNull::slice_from_raw_parts(ptr, layout.size()))
    }
}

// SAFETY: `allocate` hands out non-overlapping blocks (each block advances the bump
// cursor past its own extent), and `deallocate` never returns inline memory to the
// pool, so no inline block can be recycled while still in use. Blocks obtained
// through the global fallback are forwarded verbatim to `Global`, which upholds its
// own contract.
unsafe impl<const N: usize> Allocator for &StackBump<N> {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self.allocate_inline(layout) {
            // The buffer can serve the request from inline storage: bump the cursor.
            Some(block) => Ok(block),
            // Too big to fit in the inline buffer, possibly after alignment padding:
            // serve the request from the heap rather than fail.
            None => Global.allocate(layout),
        }
    }

    /// No-op for inline blocks: the whole buffer is reclaimed together with the
    /// `StackBump`, and inline blocks are never recycled individually. Blocks
    /// obtained through the global-allocator fallback are freed back to it.
    ///
    /// # Safety
    /// Same contract as [`Allocator::deallocate`]: `ptr` must have been returned by a
    /// previous [`Allocator::allocate`] call on this allocator for `layout`.
    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if self.is_inline(ptr, layout) {
            return;
        }
        // SAFETY: `ptr` was granted by `Global::allocate` with this exact layout (it
        // lies outside the inline buffer, so it can only come from the fallback).
        unsafe { Global.deallocate(ptr, layout) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allocator_api2::vec::Vec as AllocVec;

    #[test]
    fn allocates_distinct_non_overlapping_aligned_blocks() {
        let bump = StackBump::<128>::new();

        let a = bump
            .allocate_inline(Layout::from_size_align(8, 8).unwrap())
            .unwrap();
        let b = bump
            .allocate_inline(Layout::from_size_align(16, 16).unwrap())
            .unwrap();

        let a_addr = a.cast::<u8>().as_ptr() as usize;
        let b_addr = b.cast::<u8>().as_ptr() as usize;
        assert_eq!(a.len(), 8);
        assert_eq!(b.len(), 16);
        assert_eq!(a_addr % 8, 0);
        assert_eq!(b_addr % 16, 0);
        // Distinct, non-overlapping ranges.
        assert!(a_addr + 8 <= b_addr);
        assert_eq!(bump.used_bytes(), b_addr + 16 - (bump.base() as usize));
        assert_eq!(bump.remaining_bytes(), 128 - bump.used_bytes());
    }

    #[test]
    fn alignment_beyond_storage_align_served_inline() {
        let bump = StackBump::<128>::new();
        let layout = Layout::from_size_align(32, 32).unwrap();

        let ptr = <&StackBump<128> as Allocator>::allocate(&&bump, layout).unwrap();
        assert_eq!(ptr.cast::<u8>().as_ptr() as usize % 32, 0);
        // The block is served inline even though 32 exceeds the buffer's own 8-byte
        // alignment: the bump rounds the absolute address up to the requested
        // alignment, consuming the block plus any alignment padding.
        assert!(bump.is_inline(ptr.cast(), layout));
        assert!(bump.used_bytes() >= 32);

        // Inline deallocation is a no-op.
        // SAFETY: the block was granted by a previous `allocate` call on this
        // allocator with this exact layout.
        unsafe { <&StackBump<128> as Allocator>::deallocate(&&bump, ptr.cast(), layout) };
    }

    #[test]
    fn alignment_padding_causes_fallback() {
        let bump = StackBump::<64>::new();
        // Consume an odd number of bytes with single-byte, align-1 requests, so the
        // cursor is not 8-aligned. The buffer being 8-aligned (`repr(align(8))`), any
        // subsequent align-32 request is then guaranteed a nonzero pad, whatever the
        // buffer's runtime address: `pad = -(base + cursor) & 31` is always in
        // `7..=31`, never 0.
        for _ in 0..57 {
            bump.allocate_inline(Layout::from_size_align(1, 1).unwrap())
                .unwrap();
        }
        assert_eq!(bump.used_bytes(), 57);

        // The 7 remaining bytes fit the block on size alone, but the alignment
        // padding doesn't: the request must be served by the global fallback, without
        // disturbing the bump cursor.
        let layout = Layout::from_size_align(7, 32).unwrap();
        let ptr = <&StackBump<64> as Allocator>::allocate(&&bump, layout).unwrap();
        assert!(!bump.is_inline(ptr.cast(), layout));
        assert_eq!(ptr.cast::<u8>().as_ptr() as usize % 32, 0);
        assert_eq!(bump.used_bytes(), 57);
        // SAFETY: the block was granted by a previous `allocate` call on this
        // allocator with this exact layout; it came from `Global`, which frees it.
        unsafe { <&StackBump<64> as Allocator>::deallocate(&&bump, ptr.cast(), layout) };
    }

    #[test]
    fn exhaustion_falls_back_to_global() {
        let bump = StackBump::<64>::new();

        let inline = <&StackBump<64> as Allocator>::allocate(
            &&bump,
            Layout::from_size_align(64, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(inline.len(), 64);
        assert_eq!(bump.used_bytes(), 64);

        // The buffer is exhausted: the next request is served by the global fallback,
        // without disturbing the bump cursor.
        let heap = <&StackBump<64> as Allocator>::allocate(
            &&bump,
            Layout::from_size_align(16, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(heap.len(), 16);
        assert_eq!(bump.used_bytes(), 64);

        // SAFETY: 16 bytes granted by the allocator, written before any other use.
        unsafe { std::ptr::write_bytes(heap.cast::<u8>().as_ptr(), 0xAB, 16) };
        // SAFETY: the block came from `Global` with this exact layout.
        unsafe {
            <&StackBump<64> as Allocator>::deallocate(
                &&bump,
                heap.cast(),
                Layout::from_size_align(16, 1).unwrap(),
            )
        };
    }

    #[test]
    fn zero_sized_layout_is_inline() {
        let bump = StackBump::<8>::new();
        let zst = <&StackBump<8> as Allocator>::allocate(&&bump, Layout::new::<()>()).unwrap();
        assert_eq!(zst.len(), 0);
        // Doesn't consume any inline space.
        assert_eq!(bump.used_bytes(), 0);
        // SAFETY: ZST deallocation is a no-op for any allocator.
        unsafe {
            <&StackBump<8> as Allocator>::deallocate(&&bump, zst.cast(), Layout::new::<()>())
        };
    }

    #[test]
    fn zst_at_exhausted_boundary_is_served_inline() {
        let bump = StackBump::<64>::new();
        // Exhaust the inline buffer.
        let inline = <&StackBump<64> as Allocator>::allocate(
            &&bump,
            Layout::from_size_align(64, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(inline.len(), 64);
        assert_eq!(bump.used_bytes(), 64);

        // A zero-sized request lands exactly one past the end of the buffer. That is a
        // valid address to hand out for a zero-sized block, so it stays inline, at
        // `base + N`, without disturbing the bump cursor.
        let zst_layout = Layout::new::<()>();
        let zst = <&StackBump<64> as Allocator>::allocate(&&bump, zst_layout).unwrap();
        assert_eq!(zst.len(), 0);
        assert_eq!(
            zst.cast::<u8>().as_ptr() as usize,
            bump.base() as usize + 64
        );
        assert_eq!(bump.used_bytes(), 64);
        // `is_inline` recognizes every inline block, including the boundary ZST, so
        // deallocation no-ops instead of forwarding to the global allocator.
        assert!(bump.is_inline(zst.cast(), zst_layout));
        // SAFETY: the block was granted by a previous `allocate` call on this
        // allocator with this exact layout.
        unsafe {
            <&StackBump<64> as Allocator>::deallocate(&&bump, zst.cast(), Layout::new::<()>())
        };

        // A subsequent non-zero request can neither reuse the boundary address nor
        // fit past it: it is served by the global fallback, without disturbing the
        // bump cursor, and its deallocation routes to `Global` through `is_inline`.
        let after_layout = Layout::from_size_align(1, 1).unwrap();
        let after = <&StackBump<64> as Allocator>::allocate(&&bump, after_layout).unwrap();
        assert!(!bump.is_inline(after.cast(), after_layout));
        assert_eq!(bump.used_bytes(), 64);
        // SAFETY: the block was granted by a previous `allocate` call on this
        // allocator with this exact layout; it came from `Global`, which frees it.
        unsafe { <&StackBump<64> as Allocator>::deallocate(&&bump, after.cast(), after_layout) };
    }

    #[test]
    fn vec_backed_by_stack() {
        let bump = StackBump::<1024>::new();
        let mut v: AllocVec<u64, &StackBump<1024>> = AllocVec::with_capacity_in(16, &bump);
        for i in 0..16u64 {
            v.push(i * 7);
        }
        assert_eq!(
            v.as_slice(),
            &(0..16u64).map(|i| i * 7).collect::<Vec<_>>()[..]
        );
        // 16 u64 = 128 bytes, all served inline: the bump cursor moved.
        assert!(bump.used_bytes() >= 128);
    }

    #[test]
    fn vec_spilling_to_heap_stays_correct() {
        let bump = StackBump::<64>::new();
        // The first 64 bytes are served inline, growth beyond that transparently
        // falls back to the heap.
        let mut v: AllocVec<u64, &StackBump<64>> = AllocVec::with_capacity_in(8, &bump);
        for i in 0..64u64 {
            v.push(i);
        }
        assert_eq!(v.as_slice(), &(0..64u64).collect::<Vec<_>>()[..]);
    }

    #[test]
    fn hashbrown_set_backed_by_stack() {
        let bump = StackBump::<1024>::new();
        let mut set = hashbrown::HashSet::new_in(&bump);
        assert!(set.try_reserve(16).is_ok());
        for i in 0..16 {
            assert!(set.insert(i));
        }
        assert_eq!(set.len(), 16);
        assert!(!set.insert(3));
        assert_eq!(set.len(), 16);
        // 16 entries of 4-byte keys + ctrl bytes: entirely inline.
        assert!(bump.used_bytes() > 0);

        // A set too big for the buffer transparently falls back to the heap.
        let small_bump = StackBump::<64>::new();
        let mut big =
            hashbrown::HashSet::<u64, hashbrown::DefaultHashBuilder, &StackBump<64>>::new_in(
                &small_bump,
            );
        for i in 0..1024u64 {
            assert!(big.insert(i));
        }
        assert_eq!(big.len(), 1024);
    }
}
