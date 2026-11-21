// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// `AllocError`, `Allocator`, and the `Allocator` impls for `&A` and `&mut A`
// are copied from the Rust core library, `library/core/src/alloc/mod.rs` in
// Rust 1.100.0 (0ef1bc386 2026-11-09), where they are stable. That code is
// Copyright The Rust Project Developers and dual-licensed under MIT OR
// Apache-2.0; it is used here under Apache-2.0.
//
// It is copied rather than used because this crate's minimum Rust version is
// older than 1.100. Changes from the original, all so it builds on 1.91:
// - Stability attributes (`#[stable]`, `#[unstable]`, `#[rustc_const_unstable]`) are removed.
// - `Allocator` is a plain `unsafe trait`, not a `const` trait, and the impls drop their `[const]`
//   bounds; const allocation is unstable in 1.100 anyway.
// - The default methods use `NonNull::cast` instead of `NonNull<[u8]>::as_non_null_ptr` and
//   `as_mut_ptr`, which are newer than 1.91.
// - `GlobalAlloc` gets an explicit doc link target, since it is not in scope here.
// - Doc links that were relative to std's docs point at doc.rust-lang.org, and the unstable
//   `AllocatorClone` is mentioned without a link.
// - The unstable `allocator_ext` items (`GlobalAllocator`, `AllocatorClone`, `StaticAllocator`,
//   `AllocatorNightly`, and the `Pin<P>` impl) are omitted.
//
// Signatures and safety contracts are unchanged, so once the minimum Rust
// version reaches 1.100 this module can re-export `core::alloc::{Allocator,
// AllocError}` instead.

//! A copy of the `core::alloc::Allocator` API as stabilized in Rust 1.100.

use core::alloc::Layout;
use core::error::Error;
use core::fmt;
use core::ptr::{self, NonNull};

// Not part of the copy: this crate's own stand-in for `alloc::alloc::Global`.
#[cfg(any(test, feature = "alloc"))]
mod global;
#[cfg(any(test, feature = "alloc"))]
pub use global::Global;

/// The `AllocError` error indicates an allocation failure
/// that may be due to resource exhaustion or to
/// something wrong when combining the given input arguments with this
/// allocator.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct AllocError;

impl Error for AllocError {}

// (we need this for downstream impl of trait Error)
impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("memory allocation failed")
    }
}

/// An implementation of `Allocator` can allocate, grow, shrink, and deallocate arbitrary blocks of
/// data described via [`Layout`][].
///
/// `Allocator` is mostly designed to be implemented on ZSTs, references, or smart pointers,
/// but can also be implemented directly on the underlying memory-owning type so long as it
/// upholds the necessary guarantees. In general, an allocator of the type `MyAlloc([u8; N])`
/// cannot be soundly created without being pinned or otherwise immovable in order to be
/// correct.
///
/// In contrast to [`GlobalAlloc`][], `Allocator` allows zero-sized allocations. If an underlying
/// allocator does not support this (like jemalloc) or responds by returning a null pointer
/// (such as `libc::malloc`), this must be caught by the implementation.
///
/// In order to be usable in a flexible manner while still being sound, implementors of the trait
/// must uphold very detailed semantics as explained below; the following terms are thus provided
/// as vocabulary for allocator safety and implementation requirements:
///
/// ### Equivalent allocators
///
/// Multiple allocator values can sometimes be interchangeable with each other.
/// When this is the case, we refer to those allocators as being *equivalent* to
/// each other.
///
/// Users of allocators may assume the following are true of equivalent allocators,
/// and implementors must ensure these rules are upheld:
/// * An allocator is equivalent to itself. (Equivalence is reflexive.)
/// * If an allocator is equivalent to a second allocator, then the second allocator is also
///   equivalent to the first. (Equivalence is symmetric.)
/// * If an allocator is equivalent to a second allocator, and the second allocator is equivalent to
///   a third allocator, then the first allocator is also equivalent to the third allocator.
///   (Equivalence is transitive.)
/// * Moving, subtyping, unsize-coercing, or trait-upcasting an allocator does not change what the
///   allocator is equivalent to.
/// * Copying or cloning an allocator creates an equivalent one, should the `AllocatorClone` trait
///   be implemented.
///
/// Additionally, implementors of `Allocator` may specify additional equivalences
/// between allocators. It is the responsibility of such implementors to make sure
/// that equivalent allocators have "compatible" `Allocator` implementations.
/// In particular, the standard library specifies the following equivalences:
/// * A reference to an allocator (either `&` or `&mut`) is equivalent to the allocator being
///   referenced.
/// * A `Box`, `Rc`, or `Arc` containing an allocator is equivalent to the allocator inside.
/// * All `Global` allocator instances are equivalent with each other.
/// * All `System` allocator instances are equivalent with each other.
///
/// ### Currently allocated memory
///
/// Some of the methods require that a memory block is *currently allocated* by some specific
/// allocator. This means that:
/// * the starting address for that memory block was previously returned by the [`allocate`],
///   [`allocate_zeroed`], [`grow`], [`grow_zeroed`], or [`shrink`] methods, called on an allocator
///   that's equivalent to this specific allocator; and
/// * the memory block has not subsequently been [*invalidated*].
///
/// ### Invalidating memory blocks
///
/// A memory block that is currently allocated becomes *invalidated* when one
/// of the following happens:
/// * The memory block is deallocated. This occurs when the memory block is passed as an argument to
///   a [`deallocate`] call, or when it is passed as an argument to a [`grow`], [`grow_zeroed`] or
///   [`shrink`] call that returns `Ok`.
/// * For all (equivalent) allocators that this memory block is currently allocated by, at least one
///   of the following has occurred:
///   * The allocator's destructor runs.
///   * The allocator is mutated through a public or otherwise untrusted API taking `&mut` access.
///   * One of the borrow-checker lifetimes in the allocator's type expires.
///
/// Note that these conditions imply that a collection may ensure that
/// any specific currently allocated memory block won't be invalidated by:
/// * not deallocating that memory block,
/// * owning an allocator that memory block is allocated with, and
/// * not publicly exposing `&mut` access to that allocator.
///
/// Also note that safe public API of an allocator with `&` access is not
/// allowed to invalidate its memory blocks. Furthermore, unsafe public API
/// of an allocator with `&` access must document that they invalidate
/// memory blocks (e.g., by calling `deallocate`) if they do. Therefore,
/// a collection may safely expose `&` access to its allocator.
///
/// Also note that, even in cases where there are other "alive" allocators known
/// to be equivalent to a given collection's allocator, most collections still should
/// not publicly expose `&mut` access to their allocators. The fact that there are
/// other "alive" allocators would prevent this `&mut` access from invalidating
/// the collection's memory block, but public `&mut` access is still likely to
/// be unsound, since a user could replace the collection's allocator with
/// a non-equivalent allocator, causing the collection to deallocate its memory
/// with the wrong allocator.
///
/// [`allocate`]: Allocator::allocate
/// [`allocate_zeroed`]: Allocator::allocate_zeroed
/// [`grow`]: Allocator::grow
/// [`grow_zeroed`]: Allocator::grow_zeroed
/// [`shrink`]: Allocator::shrink
/// [`deallocate`]: Allocator::deallocate
/// [`GlobalAlloc`]: core::alloc::GlobalAlloc
///
/// ### Memory fitting
///
/// Some of the methods require that a `layout` *fits* a memory block or vice versa. This means
/// that the following conditions must hold:
///  * the memory block must be *currently allocated* by the allocator,
///  * [`layout.align()`] must be the same as the alignment of the layout used to allocate the
///    block, and
///  * [`layout.size()`] must fall in the range `min ..= max`, where:
///    - `min` is the size of the layout used to allocate the block, and
///    - `max` is the actual size returned from [`allocate`], [`allocate_zeroed`], [`grow`],
///      [`grow_zeroed`], or [`shrink`].
///
/// [`layout.align()`]: Layout::align
/// [`layout.size()`]: Layout::size
///
/// # Safety
///
/// Implementors of `Allocator` must ensure that a memory block that
/// is [*currently allocated*] by the allocator points to valid memory
/// until that memory block is [*invalidated*]. The implementor must also
/// not violate this invariant of `Allocator` via allocator equivalences
/// that are in the implementor's control.
///
/// Additionally, any memory block returned by the allocator must
/// satisfy the allocation invariants described in `core::ptr`.
/// In particular, if a block has base address `p` and size `n`,
/// then `p as usize + n <= usize::MAX` must hold. These blocks must also
/// be wholly disjoint.
///
/// This ensures that pointer arithmetic within the allocation
/// (for example, `ptr.add(len)`) cannot overflow the address space, and
/// that it is possible to perform nonoverlapping copies between allocations.
///
/// None of the allocating or deallocating methods may unwind. This restriction
/// may be lifted in the future by ensuring unwinding out of an allocating function always
/// aborts. If an implementor of `Allocator` also has drop glue or directly implements `Drop`,
/// dropping the allocator must not result in an unwind.
///
/// It is undefined behavior for the allocator to read, write, or deallocate any memory that
/// is currently allocated. This memory is owned by the user; the allocator must not touch it.
///
/// Lastly, the methods on this trait must be *correct*; in particular, the layout requested
/// must be respected, calls must zero out memory if the documentation so requires,
/// returning an `AllocError` from a reallocating method must indeed ensure that
/// the old pointer was not invalidated, and de/reallocating calls must accept layouts
/// in the ranges defined by their documentation.
///
/// [*currently allocated*]: #currently-allocated-memory
/// [*invalidated*]: #invalidating-memory-blocks
// NOTE: the above bound on allocating methods not unwinding, alongside the similar
// bound on `AllocatorClone`, are currently load-bearing in std! see the below issues
// and make sure they cannot be triggered before relaxing this:
// https://rust.tf/156490
// https://rust.tf/159982
pub unsafe trait Allocator {
    /// Attempts to allocate a block of memory.
    ///
    /// On success, returns a [`NonNull<[u8]>`][NonNull] meeting the size and alignment
    /// guarantees of `layout`. The returned block may have a larger size than specified
    /// by `layout.size()`, and may or may not have its contents initialized.
    ///
    /// It is recommended that overallocating as per the above is only performed if doing so
    /// is cheap; there is no guarantee that the caller is able to take advantage of the
    /// returned excess. Implementors are free to e.g. provide an alternate method to query
    /// available excess if doing so is expensive and should be left to the caller.
    ///
    /// Note that the returned block of memory is considered [*currently allocated*]
    /// with this allocator (and equivalent allocators).
    /// Therefore, it is the responsibility of implementors of `Allocator` to make sure that
    /// this block of memory remains valid until it is [*invalidated*].
    ///
    /// [*currently allocated*]: #currently-allocated-memory
    /// [*invalidated*]: #invalidating-memory-blocks
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or `layout` does not meet
    /// allocator's size or alignment constraints.
    ///
    /// Implementations are encouraged to return `Err` on memory exhaustion rather than
    /// aborting, but this is not a strict requirement. (Specifically: it is *legal* to implement
    /// this trait atop an underlying native allocation library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an allocation error are encouraged to
    /// call the [`handle_alloc_error`] function, rather than directly invoking `panic!` or similar.
    ///
    /// [`handle_alloc_error`]: https://doc.rust-lang.org/alloc/alloc/fn.handle_alloc_error.html
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError>;

    /// Behaves like `allocate`, but also ensures that the returned memory is zero-initialized.
    ///
    /// # Errors
    ///
    /// Returning `Err` indicates that either memory is exhausted or `layout` does not meet
    /// allocator's size or alignment constraints.
    ///
    /// Implementations are encouraged to return `Err` on memory exhaustion rather than
    /// aborting, but this is not a strict requirement. (Specifically: it is *legal* to implement
    /// this trait atop an underlying native allocation library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an allocation error are encouraged to
    /// call the [`handle_alloc_error`] function, rather than directly invoking `panic!` or similar.
    ///
    /// [`handle_alloc_error`]: https://doc.rust-lang.org/alloc/alloc/fn.handle_alloc_error.html
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let ptr = self.allocate(layout)?;
        // SAFETY: `alloc` returns a valid memory block
        unsafe { ptr.cast::<u8>().as_ptr().write_bytes(0, ptr.len()) }
        Ok(ptr)
    }

    /// Deallocates the memory referenced by `ptr`.
    ///
    /// # Safety
    ///
    /// * `ptr` must denote a block of memory [*currently allocated*] via this allocator, and
    /// * `layout` must [*fit*] that block of memory.
    ///
    /// Note that it is *immediate* language UB for a deallocation or reallocation to
    /// invalidate any outstanding references, smart pointers, etc.; thus, notably, an
    /// allocator that has been moved into its own [*currently allocated*] memory may
    /// not have its backing memory be freed, even if the allocator is never used again
    /// afterwards. This is due to the fact that such a deallocation would invalidate the
    /// `&self` reference passed to this method.
    ///
    /// [*currently allocated*]: #currently-allocated-memory
    /// [*fit*]: #memory-fitting
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout);

    /// Attempts to extend the memory block.
    ///
    /// Returns a new [`NonNull<[u8]>`][NonNull] containing a pointer and the actual size of the
    /// allocated memory. The pointer is suitable for holding data described by `new_layout`. To
    /// accomplish this, the allocator may extend the allocation referenced by `ptr` to fit the
    /// new layout.
    ///
    /// If this returns `Ok`, then the memory block referenced by `ptr` has been [*invalidated*].
    /// The old `ptr` must not be used to access the memory, even if the allocation was grown
    /// in-place. The newly returned pointer is the only valid pointer for accessing this memory
    /// now. All bytes past `old_layout.size()` should be assumed to be uninitialised.
    ///
    /// If this method returns `Err`, then the memory block has not been *invalidated*,
    /// and the contents of the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// * `ptr` must denote a block of memory [*currently allocated*] via this allocator.
    /// * `old_layout` must [*fit*] that block of memory (The `new_layout` argument need not fit
    ///   it.).
    /// * `new_layout.size()` must be greater than or equal to `old_layout.size()`.
    ///
    /// Note that `new_layout.align()` need not be the same as `old_layout.align()`.
    ///
    /// [*currently allocated*]: #currently-allocated-memory
    /// [*fit*]: #memory-fitting
    /// [*invalidated*]: #invalidating-memory-blocks
    ///
    /// # Errors
    ///
    /// Returns `Err` if the new layout does not meet the allocator's size and alignment
    /// constraints of the allocator, or if growing otherwise fails.
    ///
    /// Implementations are encouraged to return `Err` on memory exhaustion rather than
    /// aborting, but this is not a strict requirement. (Specifically: it is *legal* to implement
    /// this trait atop an underlying native allocation library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an allocation error are encouraged to
    /// call the [`handle_alloc_error`] function, rather than directly invoking `panic!` or similar.
    ///
    /// [`handle_alloc_error`]: https://doc.rust-lang.org/alloc/alloc/fn.handle_alloc_error.html
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new_ptr = self.allocate(new_layout)?;

        // SAFETY: because `new_layout.size()` must be greater than or equal to
        // `old_layout.size()`, both the old and new memory allocation are valid for reads and
        // writes for `old_layout.size()` bytes. Also, because the old allocation wasn't yet
        // deallocated, it cannot overlap `new_ptr`. Thus, the call to `copy_nonoverlapping` is
        // safe. The safety contract for `dealloc` must be upheld by the caller.
        unsafe {
            ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                new_ptr.cast::<u8>().as_ptr(),
                old_layout.size(),
            );
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }

    /// Behaves like `grow`, but also ensures that the new contents are set to zero before being
    /// returned.
    ///
    /// The memory block will contain the following contents after a successful call to
    /// `grow_zeroed`:
    ///   * Bytes `0..old_layout.size()` are preserved from the original allocation.
    ///   * Bytes `old_layout.size()..new_size` are zeroed. `new_size` refers to the size of the
    ///     memory block returned by the `grow_zeroed` call, which may be larger than
    ///     `new_layout.size()`.
    ///
    /// # Safety
    ///
    /// * `ptr` must denote a block of memory [*currently allocated*] via this allocator.
    /// * `old_layout` must [*fit*] that block of memory (The `new_layout` argument need not fit
    ///   it.).
    /// * `new_layout.size()` must be greater than or equal to `old_layout.size()`.
    ///
    /// Note that `new_layout.align()` need not be the same as `old_layout.align()`.
    ///
    /// [*currently allocated*]: #currently-allocated-memory
    /// [*fit*]: #memory-fitting
    ///
    /// # Errors
    ///
    /// Returns `Err` if the new layout does not meet the allocator's size and alignment
    /// constraints of the allocator, or if growing otherwise fails.
    ///
    /// Implementations are encouraged to return `Err` on memory exhaustion rather than
    /// aborting, but this is not a strict requirement. (Specifically: it is *legal* to implement
    /// this trait atop an underlying native allocation library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an allocation error are encouraged to
    /// call the [`handle_alloc_error`] function, rather than directly invoking `panic!` or similar.
    ///
    /// [`handle_alloc_error`]: https://doc.rust-lang.org/alloc/alloc/fn.handle_alloc_error.html
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        debug_assert!(
            new_layout.size() >= old_layout.size(),
            "`new_layout.size()` must be greater than or equal to `old_layout.size()`"
        );

        let new_ptr = self.allocate_zeroed(new_layout)?;

        // SAFETY: because `new_layout.size()` must be greater than or equal to
        // `old_layout.size()`, both the old and new memory allocation are valid for reads and
        // writes for `old_layout.size()` bytes. Also, because the old allocation wasn't yet
        // deallocated, it cannot overlap `new_ptr`. Thus, the call to `copy_nonoverlapping` is
        // safe. The safety contract for `dealloc` must be upheld by the caller.
        unsafe {
            ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                new_ptr.cast::<u8>().as_ptr(),
                old_layout.size(),
            );
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }

    /// Attempts to shrink the memory block.
    ///
    /// Returns a new [`NonNull<[u8]>`][NonNull] containing a pointer and the actual size of the
    /// allocated memory. The pointer is suitable for holding data described by `new_layout`. To
    /// accomplish this, the allocator may shrink the allocation referenced by `ptr` to fit the
    /// new layout.
    ///
    ///
    /// If this returns `Ok`, then the memory block referenced by `ptr` has been [*invalidated*].
    /// The old `ptr` must not be used to access the memory, even if the allocation was shrunk
    /// in-place. The newly returned pointer is the only valid pointer for accessing this memory
    /// now. All bytes past `new_layout.size()` should be assumed to be uninitialised.
    ///
    /// If this method returns `Err`, then the memory block has not been *invalidated*,
    /// and the contents of the memory block are unaltered.
    ///
    /// # Safety
    ///
    /// * `ptr` must denote a block of memory [*currently allocated*] via this allocator.
    /// * `old_layout` must [*fit*] that block of memory (The `new_layout` argument need not fit
    ///   it.).
    /// * `new_layout.size()` must be smaller than or equal to `old_layout.size()`.
    ///
    /// Note that `new_layout.align()` need not be the same as `old_layout.align()`.
    ///
    /// [*currently allocated*]: #currently-allocated-memory
    /// [*fit*]: #memory-fitting
    /// [*invalidated*]: #invalidating-memory-blocks
    ///
    /// # Errors
    ///
    /// Returns `Err` if the new layout does not meet the allocator's size and alignment
    /// constraints of the allocator, or if shrinking otherwise fails.
    ///
    /// Implementations are encouraged to return `Err` on memory exhaustion rather than
    /// aborting, but this is not a strict requirement. (Specifically: it is *legal* to implement
    /// this trait atop an underlying native allocation library that aborts on memory exhaustion.)
    ///
    /// Clients wishing to abort computation in response to an allocation error are encouraged to
    /// call the [`handle_alloc_error`] function, rather than directly invoking `panic!` or similar.
    ///
    /// [`handle_alloc_error`]: https://doc.rust-lang.org/alloc/alloc/fn.handle_alloc_error.html
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

        let new_ptr = self.allocate(new_layout)?;

        // SAFETY: because `new_layout.size()` must be lower than or equal to
        // `old_layout.size()`, both the old and new memory allocation are valid for reads and
        // writes for `new_layout.size()` bytes. Also, because the old allocation wasn't yet
        // deallocated, it cannot overlap `new_ptr`. Thus, the call to `copy_nonoverlapping` is
        // safe. The safety contract for `dealloc` must be upheld by the caller.
        unsafe {
            ptr::copy_nonoverlapping(
                ptr.as_ptr(),
                new_ptr.cast::<u8>().as_ptr(),
                new_layout.size(),
            );
            self.deallocate(ptr, old_layout);
        }

        Ok(new_ptr)
    }
}

unsafe impl<A> Allocator for &A
where
    A: Allocator + ?Sized,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        (**self).allocate(layout)
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        (**self).allocate_zeroed(layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).deallocate(ptr, layout) }
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).grow(ptr, old_layout, new_layout) }
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).grow_zeroed(ptr, old_layout, new_layout) }
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).shrink(ptr, old_layout, new_layout) }
    }
}

unsafe impl<A> Allocator for &mut A
where
    A: Allocator + ?Sized,
{
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        (**self).allocate(layout)
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        (**self).allocate_zeroed(layout)
    }

    #[inline]
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).deallocate(ptr, layout) }
    }

    #[inline]
    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).grow(ptr, old_layout, new_layout) }
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).grow_zeroed(ptr, old_layout, new_layout) }
    }

    #[inline]
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // SAFETY: the safety contract must be upheld by the caller
        unsafe { (**self).shrink(ptr, old_layout, new_layout) }
    }
}
