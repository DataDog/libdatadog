// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A bump allocator over inline (stack-resident) storage, implementing the
//! `allocator_api_2` [`Allocator`] trait.
//!
//! [`StackBump`] exists so that short-lived, single-scope scratch collections can be allocated out
//! of  a buffer living on the caller's stack, avoiding any heap traffic for the common
//! small-map case.
//!
//! Requests the inline buffer can't serve (too big to fit, or an alignment above 16)
//! transparently fall back to the global (heap) allocator, and such blocks are freed
//! on deallocation. An under-sized buffer therefore only costs performance, never
//! correctness.

use allocator_api2::alloc::{AllocError, Allocator, Global};
use core::alloc::Layout;
use core::cell::{Cell, UnsafeCell};
use core::mem::MaybeUninit;
use core::ptr::NonNull;

/// Alignment of the inline buffer. Requests for a higher alignment are served by the
/// global-allocator fallback, since a pointer bump can only honor alignments up to the
/// buffer's own alignment. 16 covers every alignment requested in practice by the
/// collections this allocator backs (`hashbrown` tables of references, `Vec<bool>`).
const MAX_INLINE_ALIGN: usize = 16;

#[repr(align(16))]
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
/// - Once the inline buffer is exhausted — or when a request asks for an alignment above 16 or more
///   bytes than remain — allocations transparently fall back to the global (heap) allocator, and
///   such blocks are freed on deallocation. An under-sized buffer therefore degrades performance,
///   never correctness.
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
    /// it. Zero-sized blocks are only ever granted strictly inside the buffer: a
    /// zero-sized request that would land exactly one past the end of the buffer is
    /// served by the global-allocator fallback instead, so every inline block,
    /// zero-sized or not, is recognized by this range check.
    #[inline]
    fn is_inline(&self, ptr: NonNull<u8>) -> bool {
        let addr = ptr.as_ptr() as usize;
        let base = self.base() as usize;
        addr >= base && addr < base.wrapping_add(N)
    }

    /// Bump-allocate `layout` out of the inline buffer, or `None` if it doesn't fit.
    fn allocate_inline(&self, layout: Layout) -> Option<NonNull<[u8]>> {
        let align = layout.align();
        // A pointer bump can't produce blocks aligned beyond the buffer's own alignment;
        // such requests are served by the global-allocator fallback in `allocate`.
        if align > MAX_INLINE_ALIGN {
            return None;
        }

        // Round the cursor up to the requested alignment. `align` being a power of two
        // and a divisor of the buffer alignment, the resulting block is properly aligned;
        // checked arithmetic rules out overflow.
        let start = self.cursor.get().checked_add(align - 1)? & !(align - 1);
        let end = start.checked_add(layout.size())?;
        // Zero-sized blocks are only handed out strictly inside the buffer: a
        // zero-sized request at the exhausted cursor would be granted exactly one
        // past the end of the buffer, an address `is_inline` doesn't recognize. Serve
        // those from the global allocator so `deallocate` can rely on `is_inline`
        // alone.
        if end > N || (layout.size() == 0 && start == N) {
            return None;
        }
        self.cursor.set(end);

        // SAFETY: `start + layout.size() == end <= N`, so the returned pointer and the
        // `layout.size()` bytes behind it are within the `N`-byte buffer owned by
        // `self`, which outlives every block it hands out (deallocation of inline
        // blocks is a no-op).
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
            // Too big to fit, or an alignment a pointer bump can't honor: serve the
            // request from the heap rather than fail.
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
        if self.is_inline(ptr) {
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
    fn alignment_beyond_inline_align_falls_back_to_global() {
        let bump = StackBump::<128>::new();
        let layout = Layout::from_size_align(32, 32).unwrap();

        let ptr = <&StackBump<128> as Allocator>::allocate(&&bump, layout).unwrap();
        assert_eq!(ptr.cast::<u8>().as_ptr() as usize % 32, 0);
        // The block came from the global fallback: nothing was consumed from the
        // inline buffer.
        assert_eq!(bump.used_bytes(), 0);

        // Heap blocks must be writable and freed back to the global allocator.
        // SAFETY: 32 bytes granted by the allocator, written before any other use.
        unsafe { std::ptr::write_bytes(ptr.cast::<u8>().as_ptr(), 0xAB, 32) };
        // SAFETY: the block was granted by `Global` with this exact layout.
        unsafe { <&StackBump<128> as Allocator>::deallocate(&&bump, ptr.cast(), layout) };
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
    fn zst_at_exhausted_boundary_falls_back_to_global() {
        let bump = StackBump::<64>::new();
        // Exhaust the inline buffer.
        let inline = <&StackBump<64> as Allocator>::allocate(
            &&bump,
            Layout::from_size_align(64, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(inline.len(), 64);
        assert_eq!(bump.used_bytes(), 64);

        // A zero-sized request would land exactly one past the end of the buffer, so
        // it is served by the global fallback: not at `base + N`, and without
        // disturbing the bump cursor.
        let zst = <&StackBump<64> as Allocator>::allocate(&&bump, Layout::new::<()>()).unwrap();
        assert_eq!(zst.len(), 0);
        assert_ne!(
            zst.cast::<u8>().as_ptr() as usize,
            bump.base() as usize + 64
        );
        assert_eq!(bump.used_bytes(), 64);
        // `is_inline` recognizes every inline block, so the boundary ZST is correctly
        // classified as a global block.
        assert!(!bump.is_inline(zst.cast()));
        // SAFETY: the block was granted by a previous `allocate` call on this
        // allocator with this exact layout; `Global::deallocate` no-ops zero-sized
        // layouts.
        unsafe {
            <&StackBump<64> as Allocator>::deallocate(&&bump, zst.cast(), Layout::new::<()>())
        };
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

    #[test]
    fn dedup_scratch_footprint_fits_stack_budget() {
        // Regression guard for the sizing of `DEDUP_STACK_BYTES` in `vec_map`: reproduce
        // the scratch allocations `VecMap::dedup` makes for a map of 32 elements — a
        // `hashbrown` set of 32 key references plus a 32-byte keep-bitmap — and check
        // both fit in a 1024-byte inline buffer.
        let keys: [u64; 32] = core::array::from_fn(|i| i as u64);
        let bump = StackBump::<1024>::new();
        {
            let mut seen = hashbrown::HashSet::new_in(&bump);
            let mut keep: AllocVec<bool, _> = AllocVec::new_in(&bump);
            // The reservations mirror the sizes `VecMap::dedup` requests for a
            // 32-element map; the assertion below then checks they were served
            // inline, so a `hashbrown` upgrade that breaks the sizing budget fails
            // here rather than silently spilling `VecMap::dedup`'s scratch to the heap.
            seen.try_reserve(keys.len())
                .expect("seen set fits the budget");
            keep.try_reserve(keys.len())
                .expect("keep bitmap fits the budget");
            // 64 hashbrown buckets (8-byte references + control bytes) plus one byte
            // per element: the set and the bitmap must both have been served inline.
            assert!(bump.used_bytes() >= 64 * 9 + keys.len());
            for k in keys.iter().rev() {
                keep.push(seen.insert(k));
            }
            assert!(keep.iter().all(|&k| k));
        }
        assert!(bump.used_bytes() > 0);
    }
}
