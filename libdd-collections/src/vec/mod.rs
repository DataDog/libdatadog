// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Unlike [`std::vec::Vec`], this [`Vec`] is panic-free in release builds as
//! long as the operations it invokes are panic-free: `T::clone`, `T::drop`,
//! closure callbacks, allocator methods, and similar user-provided code.
//! It also exposes only a safe API, even if this means it's less efficient.
//!
//! This guarantee applies to the operations provided directly by this type. If
//! code obtains a slice view, standard slice APIs keep their standard behavior.
//! In particular, indexing and slicing the returned slice can panic on
//! out-of-bounds access.
//!
//! The API tries to match the [`std::vec::Vec`] as much as possible, although
//! it does match some currently unstable APIs which may change in the future,
//! such as [`Vec::push_within_capacity`]. However, there are behavior
//! differences, such as when growing arrays as this is meant to take advantage
//! of allocator excess.

#[cfg(all(test, feature = "alloc"))]
mod tests;

use crate::alloc::{Allocator, Layout};
use crate::{TryReserveError, TryReserveErrorKind};
use core::borrow::{Borrow, BorrowMut};
use core::fmt;
use core::hash::{Hash, Hasher};
use core::num::NonZeroUsize;
use core::ptr::NonNull;

#[cfg(feature = "alloc")]
use crate::alloc::{Box, Global};

/// `Vec` is a contiguous growable array type.
#[cfg(feature = "alloc")]
pub struct Vec<T, A: Allocator = Global> {
    ptr: NonNull<T>,
    capacity: usize,
    len: usize,
    allocator: A,
}

/// `Vec` is a contiguous growable array type.
#[cfg(not(feature = "alloc"))]
pub struct Vec<T, A: Allocator> {
    ptr: NonNull<T>,
    capacity: usize,
    len: usize,
    allocator: A,
}

/// An iterator that moves out of a [`Vec`].
pub struct IntoIter<T, A: Allocator> {
    ptr: NonNull<T>,
    capacity: usize,
    start: usize,
    end: usize,
    allocator: A,
}

#[cfg(feature = "alloc")]
impl<T> Vec<T, Global> {
    pub const fn new() -> Self {
        Self::new_in(Global)
    }

    pub fn try_with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        Self::try_with_capacity_in(capacity, Global)
    }
}

#[cfg(feature = "alloc")]
impl<T> Default for Vec<T, Global> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, A: Allocator> Vec<T, A> {
    /// Constructs a new, empty `Vec<T, A>`.
    ///
    /// The vector will not allocate until elements are pushed onto it.
    pub const fn new_in(allocator: A) -> Vec<T, A> {
        let capacity = if size_of::<T>() == 0 { usize::MAX } else { 0 };

        Vec {
            ptr: NonNull::dangling(),
            capacity,
            len: 0,
            allocator,
        }
    }

    /// Constructs a new, empty `Vec<T, A>` with at least the specified capacity
    /// with the provided allocator.
    ///
    /// The vector will be able to hold at least `capacity` elements without
    /// reallocating. This method is allowed to allocate for more elements than
    /// `capacity`. If `capacity` is zero, the vector will not allocate.
    pub fn try_with_capacity_in(
        capacity: usize,
        allocator: A,
    ) -> Result<Vec<T, A>, TryReserveError> {
        if capacity == 0 {
            return Ok(Vec::new_in(allocator));
        }

        let Some(elem_size) = NonZeroUsize::new(size_of::<T>()) else {
            return Ok(Vec::new_in(allocator));
        };

        let (ptr, capacity) = try_allocate_raw(&allocator, capacity, elem_size, align_of::<T>())?;

        Ok(Vec {
            ptr: ptr.cast(),
            capacity,
            len: 0,
            allocator,
        })
    }

    /// Constructs a new `Vec<T, A>` by cloning the elements from a slice.
    ///
    /// This is a convenience function. It reserves capacity with
    /// [`Vec::try_reserve_exact`] before calling
    /// [`Vec::extend_from_slice_within_capacity`].
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn try_from_slice_in(source: &[T], allocator: A) -> Result<Vec<T, A>, TryReserveError>
    where
        T: Clone,
    {
        let mut vec = Vec::new_in(allocator);
        vec.try_reserve_exact(source.len())?;
        let rest = vec.extend_from_slice_within_capacity(source);
        debug_assert!(rest.is_empty());
        Ok(vec)
    }

    /// Returns the total number of elements the vector can hold without
    /// reallocating.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of elements in the vector, also referred to
    /// as its 'length'.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the vector contains no elements.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns a reference to the underlying allocator.
    pub const fn allocator(&self) -> &A {
        &self.allocator
    }

    /// Extracts a slice containing the entire vector.
    pub const fn as_slice(&self) -> &[T] {
        // SAFETY: elements in the range 0 to len are initialized.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// Extracts a mutable slice of the entire vector.
    pub const fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: elements in the range 0 to len are initialized.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.as_slice().iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, T> {
        self.as_mut_slice().iter_mut()
    }

    /// Returns a reference to an element or subslice depending on the type of
    /// index.
    ///
    /// - If given a position, returns a reference to the element at that
    ///   position or `None` if out of bounds.
    /// - If given a range, returns the subslice corresponding to that range,
    ///   or `None` if out of bounds.
    pub fn get<I>(&self, index: I) -> Option<&I::Output>
    where
        I: core::slice::SliceIndex<[T]>,
    {
        self.as_slice().get(index)
    }

    /// Returns a mutable reference to an element or subslice depending on the
    /// type of index (see [`get`]) or `None` if the index is out of bounds.
    ///
    /// [`get`]: slice::get
    pub fn get_mut<I>(&mut self, index: I) -> Option<&mut I::Output>
    where
        I: core::slice::SliceIndex<[T]>,
    {
        self.as_mut_slice().get_mut(index)
    }

    /// Returns a reference to the first element, or `None` if the vector is
    /// empty.
    pub fn first(&self) -> Option<&T> {
        self.as_slice().first()
    }

    /// Returns a mutable reference to the first element, or `None` if the
    /// vector is empty.
    pub fn first_mut(&mut self) -> Option<&mut T> {
        self.as_mut_slice().first_mut()
    }

    /// Returns a reference to the last element, or `None` if the vector is
    /// empty.
    pub fn last(&self) -> Option<&T> {
        self.as_slice().last()
    }

    /// Returns a mutable reference to the last element, or `None` if the
    /// vector is empty.
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.as_mut_slice().last_mut()
    }

    /// Returns the first element and the rest of the vector, or `None` if it
    /// is empty.
    pub fn split_first(&self) -> Option<(&T, &[T])> {
        self.as_slice().split_first()
    }

    /// Returns the first element and the rest of the vector as mutable
    /// references, or `None` if it is empty.
    pub fn split_first_mut(&mut self) -> Option<(&mut T, &mut [T])> {
        self.as_mut_slice().split_first_mut()
    }

    /// Returns the last element and the rest of the vector, or `None` if it is
    /// empty.
    pub fn split_last(&self) -> Option<(&T, &[T])> {
        self.as_slice().split_last()
    }

    /// Returns the last element and the rest of the vector as mutable
    /// references, or `None` if it is empty.
    pub fn split_last_mut(&mut self) -> Option<(&mut T, &mut [T])> {
        self.as_mut_slice().split_last_mut()
    }

    /// Divides the vector into two slices at an index, or returns `None` if
    /// `mid > len`.
    pub fn split_at_checked(&self, mid: usize) -> Option<(&[T], &[T])> {
        self.as_slice().split_at_checked(mid)
    }

    /// Divides the vector into two mutable slices at an index, or returns
    /// `None` if `mid > len`.
    pub fn split_at_mut_checked(&mut self, mid: usize) -> Option<(&mut [T], &mut [T])> {
        self.as_mut_slice().split_at_mut_checked(mid)
    }

    /// Tries to reserve capacity for at least `additional` more elements to
    /// be inserted into the vec. The collection may reserve more space to
    /// speculatively avoid frequent reallocations, and will use any extra
    /// capacity returned by the allocator. After calling `try_reserve`,
    /// capacity will be greater than or equal to `self.len() + additional` if
    /// it returns `Ok(())`. Does nothing if capacity is already sufficient.
    /// This method preserves the contents even if an error occurs.
    #[inline(always)]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        if additional <= self.remaining_capacity() {
            Ok(())
        } else {
            // SAFETY: checked above that the remaining capacity is too small.
            unsafe { self.try_grow(additional) }
        }
    }

    /// Tries to reserve the minimum capacity for at least additional elements
    /// to be inserted in the given `Vec`. Unlike [`Vec::try_reserve`], this
    /// will not deliberately over-allocate to speculatively avoid frequent
    /// allocations. However, it _will_ utilize any excess capacity the
    /// allocator provides.
    #[inline(always)]
    pub fn try_reserve_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        if additional <= self.remaining_capacity() {
            Ok(())
        } else {
            // SAFETY: checked above that the remaining capacity is too small.
            unsafe { self.try_grow_exact(additional) }
        }
    }

    /// Attempts to shrink the capacity of the vector as much as possible.
    ///
    /// The behavior of this method depends on the allocator, which may either
    /// shrink the vector in-place or reallocate. The resulting vector may still
    /// have some excess capacity.
    ///
    /// If the allocator fails to shrink the allocation, the vector is left
    /// unchanged.
    pub fn try_shrink_to_fit(&mut self) -> Result<(), TryReserveError> {
        if self.capacity > self.len {
            if size_of::<T>() == 0 {
                Ok(())
            } else {
                self.try_shrink(self.len)
            }
        } else {
            Ok(())
        }
    }

    /// Attempts to shrink the capacity of the vector with a lower bound.
    ///
    /// The capacity will remain at least as large as both the length and the
    /// supplied value.
    ///
    /// If the current capacity is less than or equal to the lower limit, this
    /// is a no-op.
    ///
    /// If the allocator fails to shrink the allocation, the vector is left
    /// unchanged.
    pub fn try_shrink_to(&mut self, min_capacity: usize) -> Result<(), TryReserveError> {
        if self.capacity > min_capacity {
            let target_capacity = self.len.max(min_capacity);

            if target_capacity >= self.capacity || size_of::<T>() == 0 {
                Ok(())
            } else {
                self.try_shrink(target_capacity)
            }
        } else {
            Ok(())
        }
    }

    /// Converts the vector into a boxed slice, discarding excess capacity.
    ///
    /// If shrinking the allocation fails, the vector is dropped and the error
    /// is returned.
    ///
    /// The allocator may retain excess memory after the shrink. The boxed
    /// slice still uses the vector's length as its layout, which is valid
    /// because the successful shrink requested that layout.
    #[cfg(feature = "alloc")]
    pub fn try_into_boxed_slice(mut self) -> Result<Box<[T], A>, TryReserveError> {
        self.try_shrink_to_fit()?;

        // SAFETY: `try_shrink_to_fit` succeeded. For non-ZSTs with excess
        // capacity, the current allocation is valid to deallocate with the
        // boxed slice layout because the successful shrink requested exactly
        // `self.len` elements. Empty and ZST boxed slices use zero-sized
        // layouts, which allocator-api2 allocators must tolerate.
        Ok(unsafe { self.into_boxed_slice_unchecked() })
    }

    /// Appends an element and returns a reference to it if there is sufficient
    /// spare capacity, otherwise an error is returned with the element.
    ///
    /// This method will not reallocate when there's insufficient capacity.
    /// The caller should use [`try_reserve`] and check that it succeeds to
    /// ensure that there is enough capacity.
    pub fn push_within_capacity(&mut self, value: T) -> Result<&mut T, T> {
        if self.len != self.capacity() {
            // SAFETY: since len != cap, this is in-range.
            let mut end = unsafe { self.ptr.add(self.len) };
            // SAFETY: the [self.len] element is the first uninitialized.
            unsafe { end.write(value) };
            self.len += 1;

            // SAFETY: initialized above, the &mut self -> &mut T bound keeps
            // it alive and prevents it from being invalidated.
            Ok(unsafe { end.as_mut() })
        } else {
            Err(value)
        }
    }

    /// Appends an element to the back of the vector.
    ///
    /// If there is insufficient capacity, this method tries to grow the
    /// vector first. If growth fails, the vector is unchanged and the element
    /// is returned.
    pub fn try_push(&mut self, value: T) -> Result<&mut T, T> {
        if self.len == self.capacity() && self.try_reserve(1).is_err() {
            return Err(value);
        }

        self.push_within_capacity(value)
    }

    /// Appends as many elements from a slice as will fit in the spare
    /// capacity, and returns the elements that did not fit.
    ///
    /// This method will not reallocate when there's insufficient capacity.
    /// The caller should use [`try_reserve`] and check that it succeeds to
    /// ensure that there is enough capacity.
    #[must_use = "the returned slice contains elements that did not fit"]
    pub fn extend_from_slice_within_capacity<'a>(&mut self, source: &'a [T]) -> &'a [T]
    where
        T: Clone,
    {
        let mut rest = source;
        let mut remaining = self.remaining_capacity().min(rest.len());

        while remaining != 0 {
            let Some((item, tail)) = rest.split_first() else {
                return rest;
            };

            let value = item.clone();

            if size_of::<T>() == 0 {
                core::mem::forget(value);
            } else {
                // SAFETY: `remaining` is bounded by the spare capacity, so
                // `self.len` is in range for an uninitialized slot.
                let end = unsafe { self.ptr.add(self.len) };
                // SAFETY: `end` points to the first uninitialized slot.
                unsafe { end.write(value) };
            }

            self.len += 1;
            rest = tail;
            remaining -= 1;
        }

        rest
    }

    /// Appends as many elements from an iterator as will fit in the spare
    /// capacity, and returns the iterator containing elements that did not fit.
    ///
    /// This method will not reallocate when there's insufficient capacity.
    /// The caller should use [`try_reserve`] and check that it succeeds to
    /// ensure that there is enough capacity.
    #[must_use = "the returned iterator may contain elements that did not fit"]
    pub fn extend_within_capacity<I>(&mut self, iter: I) -> I::IntoIter
    where
        I: IntoIterator<Item = T>,
    {
        let mut iter = iter.into_iter();
        let mut remaining = self.remaining_capacity();

        while remaining != 0 {
            let Some(value) = iter.next() else {
                return iter;
            };

            if size_of::<T>() == 0 {
                core::mem::forget(value);
            } else {
                // SAFETY: `remaining` is bounded by the spare capacity, so
                // `self.len` is in range for an uninitialized slot.
                let end = unsafe { self.ptr.add(self.len) };
                // SAFETY: `end` points to the first uninitialized slot.
                unsafe { end.write(value) };
            }

            self.len += 1;
            remaining -= 1;
        }

        iter
    }

    /// Resizes the vector in-place so that `len` is equal to `new_len`.
    ///
    /// If `new_len` is greater than `len`, the vector is extended by cloning
    /// `value` into the new slots. If `new_len` is less than `len`, the vector
    /// is truncated.
    ///
    /// If allocation fails, the vector is left unchanged and `value` is
    /// dropped.
    pub fn try_resize(&mut self, new_len: usize, value: T) -> Result<(), TryReserveError>
    where
        T: Clone,
    {
        let len = self.len;

        if new_len <= len {
            self.truncate(new_len);
            return Ok(());
        }

        let additional = new_len - len;
        self.try_reserve(additional)?;

        let mut remaining = additional;
        while remaining > 1 {
            let value = value.clone();

            if size_of::<T>() == 0 {
                core::mem::forget(value);
            } else {
                // SAFETY: `try_reserve` above ensured enough spare capacity
                // for all `additional` elements. `self.len` advances only
                // after each initialized slot.
                let end = unsafe { self.ptr.add(self.len) };
                // SAFETY: `end` points to the first uninitialized slot.
                unsafe { end.write(value) };
            }

            self.len += 1;
            remaining -= 1;
        }

        if size_of::<T>() == 0 {
            core::mem::forget(value);
        } else {
            // SAFETY: one reserved uninitialized slot remains.
            let end = unsafe { self.ptr.add(self.len) };
            // SAFETY: `end` points to the first uninitialized slot.
            unsafe { end.write(value) };
        }

        self.len += 1;
        Ok(())
    }

    /// Removes the last element from the vector and returns it, or `None` if
    /// it is empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            // SAFETY: `self.len` now points to the last initialized element.
            let end = unsafe { self.ptr.as_ptr().add(self.len) };
            // SAFETY: `end` points to the last initialized element.
            Some(unsafe { core::ptr::read(end) })
        }
    }

    /// Removes and returns the last element from the vector if the predicate
    /// returns `true`, or `None` if the predicate returns `false` or the vector
    /// is empty.
    ///
    /// The predicate is not called when the vector is empty.
    pub fn pop_if(&mut self, predicate: impl FnOnce(&mut T) -> bool) -> Option<T> {
        let last = self.last_mut()?;

        if predicate(last) {
            self.pop()
        } else {
            None
        }
    }

    /// Shortens the vector, keeping the first `len` elements and dropping the
    /// rest.
    ///
    /// If `len` is greater than or equal to the vector's current length, this
    /// has no effect.
    ///
    /// This method has no effect on the allocated capacity of the vector.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len {
            return;
        }

        let old_len = self.len;
        self.len = len;

        // SAFETY: `len < old_len`, so `len` points at the first initialized
        // element being removed.
        let tail_start = unsafe { self.ptr.add(len) };
        let tail = NonNull::slice_from_raw_parts(tail_start, old_len - len);
        // SAFETY: elements in the range `len..old_len` were initialized. The
        // vector length was set to `len` first so they cannot be dropped twice.
        unsafe { core::ptr::drop_in_place(tail.as_ptr()) };
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, removes all elements `e` for which `f(&e)` returns
    /// `false`. This method operates in place, visiting each element exactly
    /// once in the original order, and preserves the order of the retained
    /// elements.
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&T) -> bool,
    {
        self.retain_mut(|element| f(element));
    }

    /// Retains only the elements specified by the predicate, passing a
    /// mutable reference to it.
    ///
    /// In other words, removes all elements `e` for which `f(&mut e)` returns
    /// `false`. This method operates in place, visiting each element exactly
    /// once in the original order, and preserves the order of the retained
    /// elements.
    pub fn retain_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(&mut T) -> bool,
    {
        let original_len = self.len;

        if original_len == 0 {
            return;
        }

        let ptr = self.ptr.as_ptr();
        let mut read = 0;

        while read < original_len {
            // SAFETY: `read` is in bounds of the initialized range.
            let current_ptr = unsafe { ptr.add(read) };
            // SAFETY: `current_ptr` points to an initialized element, and no
            // other reference to it exists.
            let keep = f(unsafe { &mut *current_ptr });

            if !keep {
                break;
            }

            read += 1;
        }

        if read == original_len {
            return;
        }

        struct RetainGuard<'a, T, A: Allocator> {
            read: usize,
            write: usize,
            original_len: usize,
            vec: &'a mut Vec<T, A>,
        }

        impl<T, A: Allocator> Drop for RetainGuard<'_, T, A> {
            fn drop(&mut self) {
                let remaining = self.original_len - self.read;
                let ptr = self.vec.ptr.as_ptr();

                // SAFETY: `read..original_len` contains initialized elements
                // that have not been inspected yet. Moving them down fills any
                // holes left by removed elements; the ranges may overlap.
                let src = unsafe { ptr.add(self.read) };
                // SAFETY: `write` is at or before `read` and in bounds for
                // the repaired vector length.
                let dst = unsafe { ptr.add(self.write) };
                // SAFETY: `src..src+remaining` and `dst..dst+remaining` are
                // in bounds and may overlap.
                unsafe { core::ptr::copy(src, dst, remaining) };

                self.vec.len = self.write + remaining;
            }
        }

        let mut guard = RetainGuard {
            read: read + 1,
            write: read,
            original_len,
            vec: self,
        };

        // SAFETY: `read` is in bounds and points to the first element rejected
        // by the predicate. The guard's read cursor has already advanced past
        // it, so a panic while dropping repairs the vector without this
        // element.
        let rejected = unsafe { ptr.add(read) };
        // SAFETY: `rejected` points to an initialized element being removed.
        unsafe { core::ptr::drop_in_place(rejected) };

        while guard.read < guard.original_len {
            // SAFETY: `read` is in bounds of the original initialized range.
            let current_ptr = unsafe { ptr.add(guard.read) };
            // SAFETY: `current_ptr` points to an initialized element, and no
            // other reference to it exists.
            let keep = f(unsafe { &mut *current_ptr });

            if keep {
                // SAFETY: `write` points at the next hole and `read > write`,
                // so source and destination are distinct slots in the same
                // allocation.
                let dst = unsafe { ptr.add(guard.write) };
                // SAFETY: `current_ptr` is initialized and `dst` is the next
                // unfilled retained slot.
                unsafe { core::ptr::copy_nonoverlapping(current_ptr, dst, 1) };

                guard.write += 1;
                guard.read += 1;
            } else {
                guard.read += 1;

                // SAFETY: `read` was advanced, so `read - 1` points to the
                // element being removed. The guard accounts for it if dropping
                // panics.
                let rejected = unsafe { ptr.add(guard.read - 1) };
                // SAFETY: `rejected` points to an initialized element being
                // removed.
                unsafe { core::ptr::drop_in_place(rejected) };
            }
        }

        guard.vec.len = guard.write;
        core::mem::forget(guard);
    }

    /// Clears the vector, removing all values.
    ///
    /// This method does not change the allocated capacity.
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Clears this vector and recycles its allocation into a new vector with a
    /// different element type.
    ///
    /// For non-zero-sized element types, `U` must have the same alignment as
    /// `T`, and `T`'s size must be a multiple of `U`'s size. This is checked
    /// at compile time. The returned vector's capacity is scaled to cover the
    /// same allocation.
    pub fn recycle<U>(mut self) -> Vec<U, A> {
        const {
            if size_of::<T>() == 0 || size_of::<U>() == 0 {
                assert!(size_of::<T>() == size_of::<U>());
            } else {
                assert!(size_of::<T>().is_multiple_of(size_of::<U>()));
                assert!(align_of::<T>() == align_of::<U>());
            }
        }

        self.clear();

        let me = core::mem::ManuallyDrop::new(self);
        let capacity = if size_of::<U>() == 0 {
            usize::MAX
        } else {
            me.capacity * (size_of::<T>() / size_of::<U>())
        };
        let ptr = if size_of::<U>() == 0 {
            NonNull::dangling()
        } else {
            me.ptr.cast()
        };

        // SAFETY: `me` will not be dropped, so moving the allocator transfers
        // ownership of it to the recycled vector.
        let allocator = unsafe { core::ptr::read(&me.allocator) };

        Vec {
            ptr,
            capacity,
            len: 0,
            allocator,
        }
    }

    /// Converts the vector into a boxed slice without reallocating.
    ///
    /// # Safety
    ///
    /// For non-zero-sized `T`, the allocation must be valid to deallocate with
    /// `Layout::array::<T>(self.len)`.
    #[cfg(feature = "alloc")]
    #[allow(dead_code)]
    unsafe fn into_boxed_slice_unchecked(self) -> Box<[T], A> {
        let me = core::mem::ManuallyDrop::new(self);
        let ptr = me.ptr;
        let len = me.len;
        let slice = NonNull::slice_from_raw_parts(ptr, len);

        // SAFETY: `me` will not be dropped, so moving the allocator transfers
        // ownership of it to the boxed slice.
        let allocator = unsafe { core::ptr::read(&me.allocator) };

        // SAFETY: guaranteed by the caller. The slice covers exactly the
        // initialized elements, and for non-ZSTs the allocation is valid to
        // deallocate with the boxed slice's layout.
        unsafe { Box::from_non_null_in(slice, allocator) }
    }

    /// Removes all but the first of consecutive elements in the vector
    /// satisfying a given equality relation.
    ///
    /// The `same_bucket` function is passed references to two elements from
    /// the vector and must determine if the elements compare equal. The
    /// elements are passed in opposite order from their order in the slice,
    /// so if `same_bucket(a, b)` returns `true`, `a` is removed.
    pub fn dedup_by<F>(&mut self, mut same_bucket: F)
    where
        F: FnMut(&mut T, &mut T) -> bool,
    {
        let len = self.len;

        if len <= 1 {
            return;
        }

        let start = self.ptr.as_ptr();
        let mut first_duplicate = 1;

        while first_duplicate < len {
            // SAFETY: `first_duplicate` and `first_duplicate - 1` are in
            // bounds because `first_duplicate` starts at 1 and is checked
            // against `len`.
            let current_ptr = unsafe { start.add(first_duplicate) };
            // SAFETY: `first_duplicate - 1` is in bounds because
            // `first_duplicate` starts at 1.
            let previous_ptr = unsafe { start.add(first_duplicate - 1) };
            // SAFETY: `current_ptr` points to an initialized element.
            let current = unsafe { &mut *current_ptr };
            // SAFETY: `previous_ptr` points to an initialized element and
            // does not alias `current_ptr`.
            let previous = unsafe { &mut *previous_ptr };
            let found_duplicate = same_bucket(current, previous);

            if found_duplicate {
                break;
            }

            first_duplicate += 1;
        }

        if first_duplicate == len {
            return;
        }

        struct FillGapOnDrop<'a, T, A: Allocator> {
            read: usize,
            write: usize,
            vec: &'a mut Vec<T, A>,
        }

        impl<T, A: Allocator> Drop for FillGapOnDrop<'_, T, A> {
            fn drop(&mut self) {
                let len = self.vec.len;
                let items_left = len - self.read;
                let dropped = self.read - self.write;

                // SAFETY: `read > write` and both are in bounds while this
                // guard is active. Copying `read..len` down to `write` fills
                // the gap left by already-dropped elements; the ranges may
                // overlap.
                let ptr = self.vec.ptr.as_ptr();
                // SAFETY: `read` is in bounds while this guard is active.
                let src = unsafe { ptr.add(self.read) };
                // SAFETY: `write` is in bounds while this guard is active.
                let dst = unsafe { ptr.add(self.write) };
                // SAFETY: `src..src+items_left` and `dst..dst+items_left`
                // are in bounds and may overlap.
                unsafe { core::ptr::copy(src, dst, items_left) };

                self.vec.len = len - dropped;
            }
        }

        let mut gap = FillGapOnDrop {
            read: first_duplicate + 1,
            write: first_duplicate,
            vec: self,
        };

        // SAFETY: `first_duplicate` is in bounds and is known to be a
        // duplicate. The guard has already advanced `read` past it, so if
        // dropping panics the vector length is repaired without this element.
        let first_duplicate_ptr = unsafe { start.add(first_duplicate) };
        // SAFETY: `first_duplicate_ptr` points to the duplicate element being
        // removed.
        unsafe { core::ptr::drop_in_place(first_duplicate_ptr) };

        while gap.read < len {
            // SAFETY: `read` is in bounds, and `write - 1` points to the last
            // kept element. Since `read > write`, these references do not
            // alias.
            let read_ptr = unsafe { start.add(gap.read) };
            // SAFETY: `write - 1` points to the last kept element.
            let previous_ptr = unsafe { start.add(gap.write - 1) };
            // SAFETY: `read_ptr` points to an initialized element.
            let current = unsafe { &mut *read_ptr };
            // SAFETY: `previous_ptr` points to an initialized element and
            // does not alias `read_ptr`.
            let previous = unsafe { &mut *previous_ptr };
            let found_duplicate = same_bucket(current, previous);

            if found_duplicate {
                gap.read += 1;

                // SAFETY: `read` was just advanced, so `read - 1` is the
                // duplicate element being removed. The guard accounts for it
                // if dropping panics.
                let duplicate_ptr = unsafe { start.add(gap.read - 1) };
                // SAFETY: `duplicate_ptr` points to the duplicate element
                // being removed.
                unsafe { core::ptr::drop_in_place(duplicate_ptr) };
            } else {
                // SAFETY: `read > write`, so source and destination are
                // distinct valid slots. The source element is moved into the
                // gap.
                let src = unsafe { start.add(gap.read) };
                // SAFETY: `write` points at the next gap slot.
                let dst = unsafe { start.add(gap.write) };
                // SAFETY: `src` and `dst` are distinct initialized/uninit
                // slots within the allocation.
                unsafe { core::ptr::copy_nonoverlapping(src, dst, 1) };

                gap.read += 1;
                gap.write += 1;
            }
        }

        gap.vec.len = gap.write;
        core::mem::forget(gap);
    }

    /// Removes all but the first of consecutive elements in the vector that
    /// resolve to the same key.
    ///
    /// If the vector is sorted, this removes all duplicates.
    pub fn dedup_by_key<F, K>(&mut self, mut key: F)
    where
        F: FnMut(&mut T) -> K,
        K: PartialEq,
    {
        self.dedup_by(|a, b| key(a) == key(b));
    }
}

impl<T, A: Allocator> AsRef<Vec<T, A>> for Vec<T, A> {
    fn as_ref(&self) -> &Vec<T, A> {
        self
    }
}

impl<T, A: Allocator> AsMut<Vec<T, A>> for Vec<T, A> {
    fn as_mut(&mut self) -> &mut Vec<T, A> {
        self
    }
}

impl<T, A: Allocator> AsRef<[T]> for Vec<T, A> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, A: Allocator> AsMut<[T]> for Vec<T, A> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T, A: Allocator> Borrow<[T]> for Vec<T, A> {
    fn borrow(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T, A: Allocator> BorrowMut<[T]> for Vec<T, A> {
    fn borrow_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: fmt::Debug, A: Allocator> fmt::Debug for Vec<T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl<T: Hash, A: Allocator> Hash for Vec<T, A> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(self.as_slice(), state);
    }
}

impl<T, U, A1, A2> PartialEq<Vec<U, A2>> for Vec<T, A1>
where
    T: PartialEq<U>,
    A1: Allocator,
    A2: Allocator,
{
    fn eq(&self, other: &Vec<U, A2>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Eq, A: Allocator> Eq for Vec<T, A> {}

impl<T, A1, A2> PartialOrd<Vec<T, A2>> for Vec<T, A1>
where
    T: PartialOrd,
    A1: Allocator,
    A2: Allocator,
{
    fn partial_cmp(&self, other: &Vec<T, A2>) -> Option<core::cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_slice())
    }
}

impl<T: Ord, A: Allocator> Ord for Vec<T, A> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<'a, T, A: Allocator> IntoIterator for &'a Vec<T, A> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T, A: Allocator> IntoIterator for &'a mut Vec<T, A> {
    type Item = &'a mut T;
    type IntoIter = core::slice::IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T, A: Allocator> IntoIterator for Vec<T, A> {
    type Item = T;
    type IntoIter = IntoIter<T, A>;

    fn into_iter(self) -> Self::IntoIter {
        let me = core::mem::ManuallyDrop::new(self);

        // SAFETY: `me` will not be dropped, so moving the allocator transfers
        // ownership of it to the iterator.
        let allocator = unsafe { core::ptr::read(&me.allocator) };

        IntoIter {
            ptr: me.ptr,
            capacity: me.capacity,
            start: 0,
            end: me.len,
            allocator,
        }
    }
}

impl<T, A: Allocator> IntoIter<T, A> {
    /// Returns a slice of the remaining elements.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: `remaining_ptr` points at the first unyielded element, and
        // `remaining_len` elements after it are still initialized.
        unsafe { core::slice::from_raw_parts(self.remaining_ptr().as_ptr(), self.remaining_len()) }
    }

    /// Returns a mutable slice of the remaining elements.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: `remaining_ptr` points at the first unyielded element, and
        // `remaining_len` elements after it are still initialized. The mutable
        // borrow of `self` prevents aliases to these elements.
        unsafe {
            core::slice::from_raw_parts_mut(self.remaining_ptr().as_ptr(), self.remaining_len())
        }
    }

    fn remaining_len(&self) -> usize {
        self.end - self.start
    }

    fn remaining_ptr(&self) -> NonNull<T> {
        if size_of::<T>() == 0 {
            self.ptr
        } else {
            // SAFETY: `start` is at most `end`, and `end` is at most the
            // vector's original length, which is within the allocation.
            unsafe { self.ptr.add(self.start) }
        }
    }
}

impl<T, A: Allocator> Iterator for IntoIter<T, A> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start == self.end {
            return None;
        }

        let ptr = self.remaining_ptr();
        self.start += 1;

        // SAFETY: `ptr` points to the first unyielded initialized element. It
        // has just been removed from the iterator's remaining range.
        Some(unsafe { core::ptr::read(ptr.as_ptr()) })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.remaining_len();
        (len, Some(len))
    }
}

impl<T, A: Allocator> DoubleEndedIterator for IntoIter<T, A> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.start == self.end {
            return None;
        }

        self.end -= 1;
        let ptr = if size_of::<T>() == 0 {
            self.ptr
        } else {
            // SAFETY: `end` was decremented from a value greater than
            // `start`, so it points to the last unyielded initialized element.
            unsafe { self.ptr.add(self.end) }
        };

        // SAFETY: `ptr` points to the last unyielded initialized element. It
        // has just been removed from the iterator's remaining range.
        Some(unsafe { core::ptr::read(ptr.as_ptr()) })
    }
}

impl<T, A: Allocator> ExactSizeIterator for IntoIter<T, A> {
    fn len(&self) -> usize {
        self.remaining_len()
    }
}

impl<T, A: Allocator> core::iter::FusedIterator for IntoIter<T, A> {}

impl<T, A: Allocator> Drop for IntoIter<T, A> {
    fn drop(&mut self) {
        struct DeallocGuard<'a, T, A: Allocator> {
            ptr: NonNull<T>,
            capacity: usize,
            allocator: &'a A,
        }

        impl<T, A: Allocator> Drop for DeallocGuard<'_, T, A> {
            fn drop(&mut self) {
                if self.capacity == 0 || size_of::<T>() == 0 {
                    return;
                }

                // SAFETY: must be true by construction, as the memory already
                // exists.
                let layout = unsafe { Layout::array::<T>(self.capacity).unwrap_unchecked() };
                // SAFETY: `ptr` belongs to this allocator, and `layout`
                // describes the original vector allocation.
                unsafe { self.allocator.deallocate(self.ptr.cast(), layout) }
            }
        }

        let _dealloc = DeallocGuard {
            ptr: self.ptr,
            capacity: self.capacity,
            allocator: &self.allocator,
        };

        let remaining = self.remaining_len();
        if remaining != 0 {
            let slice = NonNull::slice_from_raw_parts(self.remaining_ptr(), remaining);
            // SAFETY: these elements remain initialized and have not been
            // yielded by the iterator.
            unsafe { core::ptr::drop_in_place(slice.as_ptr()) };
        }
    }
}

impl<T, A: Allocator> Vec<T, A> {
    const MIN_NON_ZERO_CAP: usize = if size_of::<T>() == 1 {
        8
    } else if size_of::<T>() <= 1024 {
        4
    } else {
        1
    };

    fn remaining_capacity(&self) -> usize {
        self.capacity - self.len
    }

    /// Tries to grow the Vec.
    ///
    /// # Safety
    ///
    /// Must be called only when `additional` is larger than the
    /// [`Vec::remaining_capacity`].
    #[cold]
    #[cfg_attr(debug_assertions, track_caller)]
    unsafe fn try_grow(&mut self, additional: usize) -> Result<(), TryReserveError> {
        debug_assert!(additional > self.remaining_capacity());
        let Some(elem_size) = NonZeroUsize::new(size_of::<T>()) else {
            return Err(TryReserveErrorKind::CapacityOverflow.into());
        };

        let (ptr, capacity) = try_grow_raw(
            &self.allocator,
            self.ptr.cast(),
            self.capacity,
            self.len,
            additional,
            elem_size,
            align_of::<T>(),
            Self::MIN_NON_ZERO_CAP,
            Growth::Amortized,
        )?;

        self.ptr = ptr.cast();
        self.capacity = capacity;
        Ok(())
    }

    /// Tries to grow the Vec exactly enough to fit `additional` more elements.
    ///
    /// The allocator may still return excess memory, which becomes usable
    /// capacity.
    ///
    /// # Safety
    ///
    /// Must be called only when `additional` is larger than the
    /// [`Vec::remaining_capacity`].
    #[cold]
    #[cfg_attr(debug_assertions, track_caller)]
    unsafe fn try_grow_exact(&mut self, additional: usize) -> Result<(), TryReserveError> {
        debug_assert!(additional > self.remaining_capacity());
        let Some(elem_size) = NonZeroUsize::new(size_of::<T>()) else {
            return Err(TryReserveErrorKind::CapacityOverflow.into());
        };

        let (ptr, capacity) = try_grow_raw(
            &self.allocator,
            self.ptr.cast(),
            self.capacity,
            self.len,
            additional,
            elem_size,
            align_of::<T>(),
            Self::MIN_NON_ZERO_CAP,
            Growth::Exact,
        )?;

        self.ptr = ptr.cast();
        self.capacity = capacity;
        Ok(())
    }

    /// Tries to shrink the Vec to the requested capacity.
    ///
    /// `target_capacity` must be at least `self.len` and less than
    /// `self.capacity`.
    #[cold]
    #[cfg_attr(debug_assertions, track_caller)]
    fn try_shrink(&mut self, target_capacity: usize) -> Result<(), TryReserveError> {
        debug_assert!(target_capacity >= self.len);
        debug_assert!(target_capacity < self.capacity);
        debug_assert_ne!(size_of::<T>(), 0);
        let Some(elem_size) = NonZeroUsize::new(size_of::<T>()) else {
            return Err(TryReserveErrorKind::CapacityOverflow.into());
        };

        let result = try_shrink_raw(
            &self.allocator,
            self.ptr.cast(),
            self.capacity,
            target_capacity,
            elem_size,
            align_of::<T>(),
        )?;

        match result {
            ShrinkResult::Deallocated => {
                self.ptr = NonNull::dangling();
                self.capacity = 0;
            }
            ShrinkResult::Allocated { ptr, capacity } => {
                self.ptr = ptr.cast();
                self.capacity = capacity;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Growth {
    Amortized,
    Exact,
}

enum ShrinkResult {
    Deallocated,
    Allocated { ptr: NonNull<u8>, capacity: usize },
}

#[cold]
#[inline(never)]
#[cfg_attr(debug_assertions, track_caller)]
fn try_allocate_raw(
    allocator: &dyn Allocator,
    capacity: usize,
    elem_size: NonZeroUsize,
    elem_align: usize,
) -> Result<(NonNull<u8>, usize), TryReserveError> {
    let layout = array_layout(capacity, elem_size, elem_align)?;
    let ptr = allocator
        .allocate(layout)
        .map_err(|_| TryReserveErrorKind::AllocError {
            layout,
            non_exhaustive: (),
        })?;

    Ok((ptr.cast(), capacity_from_alloc_len(ptr.len(), elem_size)))
}

#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
#[cfg_attr(debug_assertions, track_caller)]
fn try_grow_raw(
    allocator: &dyn Allocator,
    ptr: NonNull<u8>,
    capacity: usize,
    len: usize,
    additional: usize,
    elem_size: NonZeroUsize,
    elem_align: usize,
    min_non_zero_cap: usize,
    growth: Growth,
) -> Result<(NonNull<u8>, usize), TryReserveError> {
    debug_assert!(additional > capacity - len);

    let Some(required_capacity) = len.checked_add(additional) else {
        return Err(TryReserveErrorKind::CapacityOverflow.into());
    };

    let new_capacity = match growth {
        Growth::Amortized => {
            let doubled_capacity = capacity.saturating_mul(2);
            required_capacity
                .max(doubled_capacity)
                .max(min_non_zero_cap)
        }
        Growth::Exact => required_capacity,
    };

    let new_layout = array_layout(new_capacity, elem_size, elem_align)?;
    let ptr = if capacity == 0 {
        allocator.allocate(new_layout)
    } else {
        let old_layout = array_layout(capacity, elem_size, elem_align)?;

        // SAFETY: ptr belongs to the current allocator, old_layout describes
        // the current allocation, and new_layout must be greater than
        // old_layout because this is called only when `additional` is larger
        // than the remaining capacity.
        unsafe { allocator.grow(ptr, old_layout, new_layout) }
    }
    .map_err(|_| TryReserveErrorKind::AllocError {
        layout: new_layout,
        non_exhaustive: (),
    })?;

    Ok((ptr.cast(), capacity_from_alloc_len(ptr.len(), elem_size)))
}

#[cold]
#[inline(never)]
#[cfg_attr(debug_assertions, track_caller)]
fn try_shrink_raw(
    allocator: &dyn Allocator,
    ptr: NonNull<u8>,
    capacity: usize,
    target_capacity: usize,
    elem_size: NonZeroUsize,
    elem_align: usize,
) -> Result<ShrinkResult, TryReserveError> {
    debug_assert!(target_capacity < capacity);

    let old_layout = array_layout(capacity, elem_size, elem_align)?;

    if target_capacity == 0 {
        // SAFETY: `ptr` belongs to this allocator, and `old_layout` describes
        // a fitting layout for the current allocation.
        unsafe { allocator.deallocate(ptr, old_layout) };
        return Ok(ShrinkResult::Deallocated);
    }

    let new_layout = array_layout(target_capacity, elem_size, elem_align)?;

    // SAFETY: `ptr` belongs to this allocator, `old_layout` describes the
    // current allocation, and `new_layout` is smaller because `target_capacity`
    // is less than `capacity`.
    let ptr = unsafe { allocator.shrink(ptr, old_layout, new_layout) }.map_err(|_| {
        TryReserveErrorKind::AllocError {
            layout: new_layout,
            non_exhaustive: (),
        }
    })?;

    let new_capacity = capacity_from_alloc_len(ptr.len(), elem_size);
    debug_assert!(new_capacity <= capacity);

    Ok(ShrinkResult::Allocated {
        ptr: ptr.cast(),
        capacity: new_capacity,
    })
}

fn array_layout(
    capacity: usize,
    elem_size: NonZeroUsize,
    elem_align: usize,
) -> Result<Layout, TryReserveError> {
    let Some(size) = capacity.checked_mul(elem_size.get()) else {
        return Err(TryReserveErrorKind::CapacityOverflow.into());
    };

    Layout::from_size_align(size, elem_align)
        .map_err(|_| TryReserveErrorKind::CapacityOverflow.into())
}

fn capacity_from_alloc_len(alloc_len: usize, elem_size: NonZeroUsize) -> usize {
    alloc_len / elem_size.get()
}

impl<T, A: Allocator> Drop for Vec<T, A> {
    fn drop(&mut self) {
        // Drop elements in 0 to len
        let slice = NonNull::slice_from_raw_parts(self.ptr, self.len);
        // SAFETY: 0 to len were properly constructed Ts.
        unsafe { core::ptr::drop_in_place(slice.as_ptr()) };

        if self.capacity == 0 || size_of::<T>() == 0 {
            return;
        }

        // Dealloc full capacity.
        // SAFETY: must be true by construction, as the memory already exists.
        let layout = unsafe { Layout::array::<T>(self.capacity).unwrap_unchecked() };
        // SAFETY: ptr is inherently non-null, belongs to this allocator, and
        // layout describes the slice.
        unsafe { self.allocator.deallocate(self.ptr.cast(), layout) }
    }
}

impl<T: PartialEq, A: Allocator> Vec<T, A> {
    /// Removes consecutive repeated elements in the vector according to the
    /// PartialEq trait implementation.
    ///
    /// If the vector is sorted, this removes all duplicates.
    pub fn dedup(&mut self) {
        self.dedup_by(|a, b| a == b)
    }
}
