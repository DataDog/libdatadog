// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A growable array whose every allocation is fallible.
//!
//! `alloc::vec::Vec` aborts the process when an allocation fails, and its
//! `push` keeps a growth path the optimizer cannot always remove. A
//! [`TryVec`] never aborts: each method that may allocate returns
//! [`AllocError`] and leaves the vector unchanged on failure.

use crate::collections::alloc::{AllocError, Allocator};
use core::alloc::Layout;
use core::borrow::Borrow;
use core::hash::{Hash, Hasher};
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::{fmt, slice};

/// Types for which all-zero bytes are a valid value, so
/// [`TryVec::try_zeroed_in`] can take them straight from zeroed memory.
///
/// # Safety
///
/// Every bit pattern of zeros must be a valid, initialized value of the type.
pub unsafe trait Zeroable: Copy {}

macro_rules! zeroable {
    ($($t:ty),*) => {
        // SAFETY: zero is a valid value of every primitive integer.
        $(unsafe impl Zeroable for $t {})*
    };
}
zeroable!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

/// A contiguous growable array in memory from `A`.
pub struct TryVec<T, A: Allocator> {
    ptr: NonNull<T>,
    cap: usize,
    len: usize,
    alloc: A,
}

// SAFETY: `TryVec` uniquely owns its elements and allocator, like `Vec<T>`.
unsafe impl<T: Send, A: Allocator + Send> Send for TryVec<T, A> {}
// SAFETY: shared access only reads elements and the allocator.
unsafe impl<T: Sync, A: Allocator + Sync> Sync for TryVec<T, A> {}

impl<T, A: Allocator> TryVec<T, A> {
    /// An empty vector; allocates nothing.
    pub const fn new_in(alloc: A) -> Self {
        const { assert!(size_of::<T>() != 0, "zero-sized elements are not supported") };
        Self {
            ptr: NonNull::dangling(),
            cap: 0,
            len: 0,
            alloc,
        }
    }

    /// An empty vector with room for `capacity` elements.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated.
    pub fn try_with_capacity_in(capacity: usize, alloc: A) -> Result<Self, AllocError> {
        let mut vec = Self::new_in(alloc);
        vec.try_reserve_exact(capacity)?;
        Ok(vec)
    }

    /// A vector of `len` zeros, from [`Allocator::allocate_zeroed`].
    ///
    /// Unlike writing zeros into a new vector, this lets the allocator hand
    /// out memory that is already zero (fresh pages from the OS) without
    /// touching it, so pages never written stay unresident.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated.
    pub fn try_zeroed_in(len: usize, alloc: A) -> Result<Self, AllocError>
    where
        T: Zeroable,
    {
        if len == 0 {
            return Ok(Self::new_in(alloc));
        }
        let layout = Layout::array::<T>(len).map_err(|_| AllocError)?;
        let block = alloc.allocate_zeroed(layout)?;
        Ok(Self {
            ptr: block.cast(),
            cap: len,
            // All-zero bytes are a valid `T` (`Zeroable`).
            len,
            alloc,
        })
    }

    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.cap
    }

    #[inline]
    #[must_use]
    pub const fn allocator(&self) -> &A {
        &self.alloc
    }

    #[inline]
    #[must_use]
    pub const fn as_slice(&self) -> &[T] {
        // SAFETY: the first `len` elements are initialized, and `ptr` is
        // non-null and aligned (dangling only when `len` is 0).
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    #[inline]
    #[must_use]
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: as in `as_slice`, with exclusive access.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Ensures room for `additional` more elements, growing geometrically.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated; nothing changes.
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), AllocError> {
        let needed = self.len.checked_add(additional).ok_or(AllocError)?;
        if needed <= self.cap {
            return Ok(());
        }
        self.grow_to(needed.max(self.cap.saturating_mul(2)).max(4))
    }

    /// Ensures room for exactly `additional` more elements.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated; nothing changes.
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), AllocError> {
        let needed = self.len.checked_add(additional).ok_or(AllocError)?;
        if needed <= self.cap {
            return Ok(());
        }
        self.grow_to(needed)
    }

    fn grow_to(&mut self, cap: usize) -> Result<(), AllocError> {
        let layout = Layout::array::<T>(cap).map_err(|_| AllocError)?;
        let block = if self.cap == 0 {
            self.alloc.allocate(layout)?
        } else {
            let old = Layout::array::<T>(self.cap).map_err(|_| AllocError)?;
            // SAFETY: `ptr` was allocated by `alloc` with `old`, and `layout`
            // is larger with the same alignment. On error nothing changes.
            unsafe { self.alloc.grow(self.ptr.cast(), old, layout)? }
        };
        self.ptr = block.cast();
        self.cap = cap;
        Ok(())
    }

    /// Appends `value`.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if growing fails; `value` is dropped and nothing
    /// else changes.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), AllocError> {
        self.try_reserve(1)?;
        self.push_within_capacity(value).map_err(|_| AllocError)
    }

    /// Appends `value` if there is spare capacity, without allocating.
    ///
    /// # Errors
    ///
    /// Hands `value` back if the vector is full.
    #[inline]
    pub const fn push_within_capacity(&mut self, value: T) -> Result<(), T> {
        if self.len >= self.cap {
            return Err(value);
        }
        // SAFETY: `len < cap`, so the slot is inside the allocation and
        // uninitialized.
        unsafe { self.ptr.add(self.len).write(value) };
        self.len += 1;
        Ok(())
    }

    /// The start of the spare capacity, for code (such as a C compressor) that
    /// writes elements directly; then [`Self::set_len`] makes them part of the
    /// vector.
    #[must_use]
    pub const fn spare_capacity_ptr(&mut self) -> *mut T {
        // SAFETY: `len <= cap`, so this is within or one past the allocation.
        unsafe { self.ptr.as_ptr().add(self.len) }
    }

    /// Sets the length.
    ///
    /// # Safety
    ///
    /// `len <= capacity()`, and the elements up to `len` are initialized.
    pub const unsafe fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    /// Removes and returns the last element.
    pub fn pop(&mut self) -> Option<T> {
        let last = self.len.checked_sub(1)?;
        self.len = last;
        // SAFETY: the element at `last` was initialized and is now outside
        // `len`, so it is read exactly once.
        Some(unsafe { self.ptr.add(last).read() })
    }

    /// Drops elements past `len`.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }
        let tail = ptr::slice_from_raw_parts_mut(
            // SAFETY: `len < self.len`, so this is within the allocation.
            unsafe { self.ptr.add(len).as_ptr() },
            self.len - len,
        );
        self.len = len;
        // SAFETY: the tail elements are initialized and now outside `len`.
        unsafe { ptr::drop_in_place(tail) };
    }

    /// Drops every element, keeping the allocation.
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Consumes the vector, returning its parts.
    #[must_use]
    pub fn into_raw_parts(self) -> (NonNull<T>, usize, usize, A) {
        let this = ManuallyDrop::new(self);
        // SAFETY: `this` is never dropped, so the allocator moves out once.
        let alloc = unsafe { ptr::read(&raw const this.alloc) };
        (this.ptr, this.len, this.cap, alloc)
    }
}

impl<T: Clone, A: Allocator> TryVec<T, A> {
    /// Appends clones of `items`.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if growing fails; nothing changes.
    pub fn try_extend_from_slice(&mut self, items: &[T]) -> Result<(), AllocError> {
        self.try_reserve(items.len())?;
        for item in items {
            // Cannot fail: room was reserved above.
            let _ = self.push_within_capacity(item.clone());
        }
        Ok(())
    }

    /// Appends `count` clones of `value`.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if growing fails; nothing changes.
    pub fn try_resize(&mut self, len: usize, value: &T) -> Result<(), AllocError> {
        if len <= self.len {
            self.truncate(len);
            return Ok(());
        }
        self.try_reserve_exact(len - self.len)?;
        while self.len < len {
            let _ = self.push_within_capacity(value.clone());
        }
        Ok(())
    }

    /// A copy of the vector in a new allocation from a clone of its
    /// allocator.
    ///
    /// # Errors
    ///
    /// [`AllocError`] if the memory cannot be allocated.
    pub fn try_clone(&self) -> Result<Self, AllocError>
    where
        A: Clone,
    {
        let mut copy = Self::try_with_capacity_in(self.len, self.alloc.clone())?;
        copy.try_extend_from_slice(self.as_slice())?;
        Ok(copy)
    }
}

impl<T, A: Allocator> Drop for TryVec<T, A> {
    fn drop(&mut self) {
        self.clear();
        if self.cap != 0
            && let Ok(layout) = Layout::array::<T>(self.cap)
        {
            // SAFETY: allocated by `alloc` with this layout.
            unsafe { self.alloc.deallocate(self.ptr.cast(), layout) };
        }
    }
}

impl<T, A: Allocator> Deref for TryVec<T, A> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, A: Allocator> DerefMut for TryVec<T, A> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T, A: Allocator> Borrow<[T]> for TryVec<T, A> {
    fn borrow(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Hash, A: Allocator> Hash for TryVec<T, A> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // As a slice, so a `&[T]` lookup finds a `TryVec<T, A>` key.
        self.as_slice().hash(state);
    }
}

impl<T: PartialEq, A: Allocator> PartialEq for TryVec<T, A> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq, A: Allocator> Eq for TryVec<T, A> {}

impl<T: fmt::Debug, A: Allocator> fmt::Debug for TryVec<T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<T, A: Allocator + Default> Default for TryVec<T, A> {
    fn default() -> Self {
        Self::new_in(A::default())
    }
}

impl<'v, T, A: Allocator> IntoIterator for &'v TryVec<T, A> {
    type Item = &'v T;
    type IntoIter = slice::Iter<'v, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

#[cfg(test)]
mod tests;
