// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use crate::{NeedsCapacity, TryReserveError};
use core::marker::PhantomData;
use core::{mem, ops, ptr, slice};

/// Note that Deref provides access to methods of `&[T]`, so you can call
/// methods like `len` and `is_empty`.
pub trait NoGrowOps<T: Copy>: ops::DerefMut<Target = [T]> {
    /// Returns the number of elements the collection can hold. Must be larger
    /// than or equal to the length.
    fn capacity(&self) -> usize;

    /// Sets the length of the collection.
    ///
    /// # Safety
    /// The len must be less than or equal to the capacity, and the first len
    /// elements of the collection must be properly initialized.
    unsafe fn set_len(&mut self, len: usize);

    /// Returns a mutable pointer to the collection's buffer, or a dangling
    /// pointer. This pointer must be valid for writes into the unused
    /// capacity, which is why it exists instead of deferring to DerefMut,
    /// which would be tagged for writes only for the already-allocated space.
    fn as_mut_ptr(&mut self) -> *mut T;

    /// Appends an element to the back of the collection without checking if
    /// there is enough capacity.
    ///
    /// # Safety
    /// There must be available capacity for the value to be pushed.
    #[inline]
    unsafe fn push_within_capacity(&mut self, value: T) {
        let len = self.len();
        let spare_capacity = self.spare_capacity_mut();
        debug_assert_ne!(spare_capacity.len(), 0);
        // todo: we could proxy to FixedCapacityBuffer to reduce duplication,
        //       but then there's an extra set_len on the buffer, and I'd want
        //       to make sure it gets elided.
        let ptr = spare_capacity.as_mut_ptr().cast::<T>();

        // SAFETY: this is valid for writing since it's in the spare capacity,
        // and will also be aligned.
        unsafe { ptr::write(ptr, value) };

        // SAFETY: just initialized the additional item, and it's required to
        // be within capacity (and this is debug checked).
        unsafe { self.set_len(len + 1) };
    }

    /// Extends the collection by appending the elements from the slice.
    ///
    /// # Safety
    /// The caller needs to ensure there is enough capacity before calling
    /// this function.
    #[inline]
    unsafe fn extend_from_slice_within_capacity(&mut self, data: &[T]) {
        let additional = data.len();
        let len = self.len();
        let spare_capacity = self.spare_capacity_mut();
        debug_assert!(additional <= spare_capacity.len());
        let begin = spare_capacity.as_mut_ptr().cast::<T>();
        // SAFETY: this doesn't overlap, not even with spare_capacity_mut,
        // because to re-insert the data, you'd have to have two mutable
        // borrows to the same underlying storage.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), begin, additional) };
        // SAFETY: just initialized `additional` new elements, and it's
        // required to be within capacity (and this is debug checked).
        unsafe { self.set_len(len + additional) };
    }

    /// Tries to append an element to the back of the collection.
    ///
    /// # Errors
    /// If there isn't capacity, an error is returned describing how much
    /// capacity there currently is, and how much is needed.
    #[inline]
    fn try_push_within_capacity(&mut self, value: T) -> Result<(), NeedsCapacity> {
        let available = self.remaining_capacity();
        if available != 0 {
            unsafe { self.push_within_capacity(value) };
            Ok(())
        } else {
            Err(NeedsCapacity {
                available,
                needed: 1,
            })
        }
    }

    /// Tries to append a slice of elements to the back of the collection. No
    /// items are appended if there isn't capacity for all of them.
    ///
    /// # Errors
    /// If there isn't capacity for all items, an error is returned describing
    /// how much capacity there currently is, and how much is needed.
    #[inline]
    fn try_extend_from_slice_within_capacity(&mut self, data: &[T]) -> Result<(), NeedsCapacity> {
        let needed = data.len();
        let available = self.remaining_capacity();
        if needed <= available {
            unsafe { self.extend_from_slice_within_capacity(data) };
            Ok(())
        } else {
            Err(NeedsCapacity { available, needed })
        }
    }

    /// Returns a `NonNull` pointer to the collection's buffer. It may be a
    /// dangling pointer if it hasn't allocated yet.
    #[inline]
    fn as_non_null(&mut self) -> ptr::NonNull<T> {
        // SAFETY: the collection should always return a non-null pointer.
        unsafe { ptr::NonNull::new_unchecked(self.as_mut_ptr()) }
    }

    /// Returns the remaining spare capacity as a slice of [mem::MaybeUninit].
    /// This can be used to fill data, and then call [Self::set_len]. The
    /// length of the slice is equal to [Self::remaining_capacity].
    #[inline]
    fn spare_capacity_mut(&mut self) -> &mut [mem::MaybeUninit<T>] {
        let ptr = unsafe {
            self.as_mut_ptr()
                .cast::<mem::MaybeUninit<T>>()
                .add(self.len())
        };
        unsafe { slice::from_raw_parts_mut(ptr, self.remaining_capacity()) }
    }

    /// Shortens the collection by keeping the first `len` elements. If `len`
    /// is greater than or equal to the number of elements currently in the
    /// collection, then this method has no effect.
    #[inline]
    fn truncate(&mut self, len: usize) {
        // SAFETY: clamped to the current length.
        unsafe { self.set_len(len.min(self.len())) };
    }

    /// Removes all elements, setting the length to 0.
    #[inline]
    fn clear(&mut self) {
        // SAFETY: 0 is always less than or equal to the capacity.
        unsafe { self.set_len(0) }
    }

    /// The number of elements that the collection has space for that are
    /// unused.
    #[inline]
    fn remaining_capacity(&self) -> usize {
        self.capacity().wrapping_sub(self.len())
    }
}

/// A vec-like object which has a fixed capacity. It borrows the storage, which
/// allows it to avoid allocators and provide many const functions (or at
/// least will be const when MSRV is bumped to 1.83+).
#[repr(C)]
pub struct FixedCapacityBuffer<'a, T: Copy + 'a> {
    // This purposefully uses the same struct layout as the VirtualVec to
    // prevent reordering operations when converting from the vec to the buf.
    ptr: ptr::NonNull<T>,
    len: usize,
    cap: usize,
    _marker: PhantomData<&'a [mem::MaybeUninit<T>]>,
}

impl<'a, T: Copy + 'a> FixedCapacityBuffer<'a, T> {
    /// Creates a new fixed-capacity buffer.
    #[inline]
    pub fn new(slice: &'a mut [mem::MaybeUninit<T>]) -> Self {
        Self {
            // SAFETY: safe from ref, just can't use NonNull::from in const.
            ptr: unsafe { ptr::NonNull::new_unchecked(slice.as_mut_ptr()).cast() },
            len: 0,
            cap: slice.len(),
            _marker: PhantomData,
        }
    }

    /// Returns the number of elements the buffer holds.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the buffer contains no elements (length is zero).
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the number of elements the buffer can hold.
    #[inline]
    pub const fn capacity(&self) -> usize {
        self.cap
    }

    /// Returns the used portion of the buffer as a slice.
    #[inline]
    pub const fn as_slice(&self) -> &'a [T] {
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len()) }
    }

    /// Returns the used portion of the buffer as a mutable slice.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &'a mut [T] {
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.len()) }
    }

    /// Provides the amount of capacity that is still remaining, that is, the
    /// capacity minus the length.
    #[inline]
    pub const fn remaining_capacity(&self) -> usize {
        self.capacity().wrapping_sub(self.len())
    }

    /// Returns the remaining spare capacity as a slice of [mem::MaybeUninit].
    /// This can be used to fill data, and then call [Self::set_len]. The
    /// length of the slice is equal to [Self::remaining_capacity].
    #[inline]
    pub fn spare_capacity_mut(&mut self) -> &mut [mem::MaybeUninit<T>] {
        let ptr = unsafe {
            self.as_mut_ptr()
                .cast::<mem::MaybeUninit<T>>()
                .add(self.len())
        };
        unsafe { slice::from_raw_parts_mut(ptr, self.remaining_capacity()) }
    }

    /// Appends an element to the back of the buffer without checking if there
    /// is enough capacity.
    ///
    /// # Safety
    /// There must be available capacity for the value to be pushed.
    #[inline]
    pub unsafe fn push_within_capacity(&mut self, value: T) {
        unsafe { self.as_mut_ptr().add(self.len()).write(value) };
        self.set_len(self.len() + 1);
    }

    /// Extends the buffer by appending the elements from the slice.
    ///
    /// # Safety
    /// The caller needs to ensure there is enough capacity before calling
    /// this function.
    #[inline]
    pub unsafe fn extend_from_slice_within_capacity(&mut self, values: &[T]) {
        let additional = values.len();
        let begin = unsafe { self.as_mut_ptr().add(self.len()) };
        // SAFETY: this doesn't overlap, not even with spare_capacity_mut,
        // because to re-insert the data, you'd have to have two mutable
        // borrows to the same underlying storage.
        ptr::copy_nonoverlapping(values.as_ptr(), begin, additional);
        self.set_len(self.len() + additional);
    }

    /// Appends an element to the back of the collection without checking if there
    /// is enough capacity.
    ///
    /// # Safety
    /// There must be available capacity for the value to be pushed.
    #[inline]
    pub fn try_push_within_capacity(&mut self, value: T) -> Result<(), NeedsCapacity> {
        let available = self.remaining_capacity();
        if available != 0 {
            unsafe { self.push_within_capacity(value) }
            Ok(())
        } else {
            Err(NeedsCapacity {
                available,
                needed: 1,
            })
        }
    }

    /// Tries to append a slice of elements to the back of the collection. No items
    /// from the slice are appended if there isn't capacity for all of them.
    ///
    /// # Errors
    /// If there isn't capacity for all items, an error is returned describing
    /// how much capacity there currently is, and how much is needed.
    #[inline]
    pub fn try_extend_from_slice_within_capacity(
        &mut self,
        data: &[T],
    ) -> Result<(), NeedsCapacity> {
        let needed = data.len();
        let available = self.remaining_capacity();
        if needed <= available {
            unsafe { self.extend_from_slice_within_capacity(data) }
            Ok(())
        } else {
            NeedsCapacity::cold_err(available, needed)
        }
    }

    /// Shortens the collection by keeping the first `len` elements. If `len` is
    /// greater than or equal to the number of elements currently in the
    /// collection, then this method has no effect.
    #[inline]
    pub fn truncate(&mut self, len: usize) {
        let min_len = self.len();
        // Avoids min to be const compatible.
        // SAFETY: clamped to the current length.
        unsafe { self.set_len(if len < min_len { len } else { min_len }) };
    }

    /// Returns `Ok(())` if there is enough capacity for `additional` elements.
    ///
    /// # Errors
    ///
    /// If there isn't enough capacity, then an error is returned.
    #[inline]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        if additional <= self.remaining_capacity() {
            Ok(())
        } else {
            Err(TryReserveError::CapacityOverflow)
        }
    }

    /// Sets the length of the buffer.
    ///
    /// # Safety
    /// The len must be less than or equal to the capacity, and the first len
    /// elements of the buffer must be properly initialized.
    #[inline]
    pub unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());
        self.len = len;
    }

    /// Returns a `NonNull` pointer to the underlying buffer. It may be a
    /// dangling pointer if it hasn't allocated yet.
    #[inline]
    pub fn as_non_null(&mut self) -> ptr::NonNull<T> {
        self.ptr
    }

    /// Returns a raw pointer to the underlying buffer. It will not be null,
    /// but it may be dangling.
    #[inline]
    pub const fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr().cast_const()
    }

    /// Returns a raw, mutable pointer to the underlying buffer. It will not
    /// be null, but it may be dangling.
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.ptr.as_ptr()
    }

    /// Returns an iterator over the used elements of the buffer.
    pub fn iter(&self) -> slice::Iter<'a, T> {
        self.as_slice().iter()
    }

    /// Returns a mutable iterator over the used elements of the buffer.
    pub fn iter_mut(&mut self) -> slice::IterMut<'a, T> {
        self.as_mut_slice().iter_mut()
    }
}

impl<'a, T: Copy> From<&'a mut [mem::MaybeUninit<T>]> for FixedCapacityBuffer<'a, T> {
    fn from(value: &'a mut [mem::MaybeUninit<T>]) -> Self {
        let cap = value.len();
        Self {
            ptr: ptr::NonNull::from(value).cast::<T>(),
            len: 0,
            cap,
            _marker: Default::default(),
        }
    }
}

impl<'a, T: Copy> From<&'a mut [T]> for FixedCapacityBuffer<'a, T> {
    fn from(value: &'a mut [T]) -> Self {
        let cap = value.len();
        Self {
            ptr: ptr::NonNull::from(value).cast::<T>(),
            len: 0,
            cap,
            _marker: Default::default(),
        }
    }
}

impl<T: Copy> NoGrowOps<T> for FixedCapacityBuffer<'_, T> {
    fn capacity(&self) -> usize {
        self.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) {
        self.set_len(len);
    }

    fn as_mut_ptr(&mut self) -> *mut T {
        self.ptr.as_ptr()
    }
}

impl<T: Copy> ops::Deref for FixedCapacityBuffer<'_, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Copy> ops::DerefMut for FixedCapacityBuffer<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

#[cfg(feature = "std")]
mod std_impls {
    use super::*;
    use std::io::{self, Write};

    impl<T: Copy> NoGrowOps<T> for std::vec::Vec<T> {
        fn capacity(&self) -> usize {
            self.capacity()
        }

        unsafe fn set_len(&mut self, len: usize) {
            self.set_len(len);
        }

        fn as_mut_ptr(&mut self) -> *mut T {
            self.as_mut_ptr()
        }
    }

    impl Write for FixedCapacityBuffer<'_, u8> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.try_extend_from_slice_within_capacity(buf) {
                Ok(_) => Ok(buf.len()),
                Err(err) => Err(io::Error::new(io::ErrorKind::OutOfMemory, err)),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slices() {
        let underlying_slice = &mut [];
        let underlying_pointer = underlying_slice.as_mut_ptr();

        let mut buffer = FixedCapacityBuffer::new(underlying_slice);
        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.capacity(), 0);
        assert!(buffer.is_empty());

        // These don't do anything, but they shouldn't panic.
        buffer.try_push_within_capacity(0).unwrap_err();
        buffer
            .try_extend_from_slice_within_capacity(&[0])
            .unwrap_err();
        buffer.try_reserve(1).unwrap_err();
        buffer.truncate(0);
        buffer.clear();

        assert_eq!(buffer.as_slice(), &[]);
        assert_eq!(buffer.as_mut_slice(), &[]);

        // The pointers should all match.
        assert!(ptr::eq(buffer.as_ptr(), underlying_pointer.cast()));
        assert!(ptr::eq(buffer.as_mut_ptr(), underlying_pointer.cast()));
        assert!(ptr::eq(
            buffer.as_non_null().as_ptr(),
            underlying_pointer.cast()
        ));

        // And of course, there is no spare capacity.
        assert_eq!(buffer.remaining_capacity(), 0);
        let space_capacity = buffer.spare_capacity_mut();
        assert_eq!(space_capacity.len(), 0);

        // No elements in the iterators.
        assert_eq!(buffer.iter().count(), 0);
    }

    #[test]
    fn stack_buffer() {
        // SAFETY: this is the unstable `transpose`--they have the same layout.
        let mut storage: [mem::MaybeUninit<u8>; 8] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; 8]>::uninit()) };

        let underlying_pointer = storage.as_ptr();
        let mut buffer = FixedCapacityBuffer::new(storage.as_mut_slice());

        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.capacity(), 8);
        assert!(buffer.is_empty());

        buffer.try_push_within_capacity(0).unwrap();
        buffer
            .try_extend_from_slice_within_capacity(&[1, 2])
            .unwrap();

        // There are now 3 elements, with 5 remaining.
        assert_eq!(buffer.as_slice(), &[0, 1, 2]);
        buffer.try_reserve(5).unwrap();
        buffer.try_reserve(6).unwrap_err();
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.capacity(), 8);
        assert_eq!(buffer.remaining_capacity(), 5);
        assert_eq!(buffer.spare_capacity_mut().as_ptr(), unsafe {
            underlying_pointer.add(3)
        });
        assert_eq!(buffer.spare_capacity_mut().len(), 5);

        // The pointers should all match.
        assert!(ptr::eq(buffer.as_ptr(), underlying_pointer.cast()));
        assert!(ptr::eq(buffer.as_mut_ptr(), underlying_pointer.cast()));
        assert!(ptr::eq(
            buffer.as_non_null().as_ptr(),
            underlying_pointer.cast()
        ));

        buffer.truncate(1);
        assert_eq!(buffer[0], 0);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.capacity(), 8);
        assert_eq!(buffer.remaining_capacity(), 7);

        buffer.clear();
        assert_eq!(buffer.len(), 0);
        assert_eq!(buffer.capacity(), 8);
        assert_eq!(buffer.remaining_capacity(), 8);
    }
}
