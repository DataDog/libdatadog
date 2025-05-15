// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::buffer::{FixedCapacityBuffer, MayGrowOps, NoGrowOps};
use crate::*;
use core::{cmp, fmt, mem, ops, ptr, slice};

/// A growable array type, similar to [std::vec::Vec]. However, it has some
/// important distinctions:
///  1. Its uses [VirtualAllocator]. This has implications:
///     - Only whole pages will be given. Once the vec moves from capacity=0,
///       it will be given at least a whole page. These pages are commonly
///       4KiB or 16KiB in size.
///     - The pages are not necessarily pre-faulted, so when the new page is
///       actually used, there will be a page fault and some cost incurred.
///  2. It does not offer APIs which can panic, although some functions are
///     unsafe. The caller needs to take precautions or use the safe versions.
///     However, it does deref to a slice, and can return an iterator, and
///     certain methods take iterators as input, and these APIs may have
///     methods which can panic, so be careful.
///  3. It can only store types which are [Copy]. This is because the
///     implementation does not wish to deal with the fact that [Clone] can
///     have undesirable side effects, as well as [Drop].
///  4. It has an FFI-safe representation, though it's not meant to be
///     manipulated directly from FFI in general. It is allowed to create a
///     [VirtualVec] with ptr=[ptr::NonNull::dangling], len=0, capacity=0.
#[repr(C)]
pub struct VirtualVec<T: Copy> {
    ptr: ptr::NonNull<T>,
    len: usize,
    capacity: usize,
}

impl<T: Copy + fmt::Debug> fmt::Debug for VirtualVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

unsafe impl<T: Send + Copy> Send for VirtualVec<T> {}

impl<T: Copy> VirtualVec<T> {
    const IS_SIZED: bool = mem::size_of::<T>() != 0;

    /// Creates a new, empty [VirtualVec]. This does not allocate and has
    /// no capacity, and is useful for const construction.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        // We don't support zero-sized types.
        const { assert!(Self::IS_SIZED) };
        Self {
            ptr: ptr::NonNull::dangling(),
            len: 0,
            capacity: 0,
        }
    }

    /// Appends an element to the back of the vec, and tries to reserve more
    /// memory if needed.
    ///
    /// # Errors
    ///
    /// If the capacity overflows, or the allocator reports a failure, then an
    /// error is returned.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), TryReserveError> {
        self.try_reserve(1)?;
        unsafe { self.push_within_capacity(value) };
        Ok(())
    }

    /// Appends an element to the back of the vec without checking if there is
    /// enough capacity.
    ///
    /// # Safety
    /// There must be available capacity for the value to be pushed.
    #[inline]
    pub unsafe fn push_within_capacity(&mut self, value: T) {
        unsafe { self.extend_from_slice_within_capacity(&[value]) };
    }

    /// Tries to append an element to the back of the vec without growing the
    /// size of the collection.
    ///
    /// # Errors
    /// If there isn't capacity, the value is returned in an error.
    #[inline]
    pub fn try_push_within_capacity(&mut self, value: T) -> Result<(), T> {
        if self.needs_to_grow(self.len(), 1) {
            Err(value)
        } else {
            unsafe { self.extend_from_slice_within_capacity(&[value]) };
            Ok(())
        }
    }

    /// Tries to reserve enough memory for at least `additional` elements. The
    /// allocator will likely reserve more memory than this to speculatively
    /// avoid frequent reallocations, as well as pad to a length in bytes that
    /// it prefers or requires. If the capacity is large enough for the
    /// requested number of additional elements, then this does nothing.
    ///
    /// # Errors
    ///
    /// If the capacity overflows, or the allocator reports a failure, then an
    /// error is returned.
    #[inline]
    pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        let len = self.len();
        if self.needs_to_grow(len, additional) {
            self.try_grow(len, additional)?
        }
        Ok(())
    }

    /// Tries to extend the collection with the contents of an [Iterator],
    /// and tries to grow the capacity of the collection if needed.
    ///
    /// Prefer [Self::try_extend_from_slice] if you have a slice or something
    /// that can deref to a slice for better performance.
    ///
    /// # Errors
    /// If there is an error while growing the capacity, an error is returned
    /// with an iterator with the remainder of the elements which have not
    /// been inserted yet.
    #[inline]
    pub fn try_extend<I>(&mut self, values: I) -> Result<(), impl Iterator>
    where
        I: IntoIterator<Item = T>,
    {
        let mut iter = values.into_iter().peekable();
        while iter.peek().is_some() {
            if self.try_reserve(1).is_err() {
                return Err(iter);
            }
            // SAFETY: checked that next() will be Some, but also ensured
            // there is enough capacity.
            unsafe { self.push_within_capacity(iter.next().unwrap_unchecked()) }
        }
        Ok(())
    }

    /// Like [Self::try_extend] except that it will not try to reserve memory
    /// if it runs out of capacity.
    ///
    /// Prefer [Self::try_extend_from_slice_within_capacity] if you have a
    /// slice or something that can deref to a slice for better performance.
    ///
    /// # Errors
    /// If the collection runs out of memory, an error is returned with an
    /// iterator with the remainder of the elements which have not been
    /// inserted yet.
    #[inline]
    pub fn try_extend_within_capacity<I>(&mut self, values: I) -> Result<(), impl Iterator>
    where
        I: IntoIterator<Item = T>,
    {
        let mut iter = values.into_iter().peekable();
        while iter.peek().is_some() {
            if self.needs_to_grow(self.len(), 1) {
                return Err(iter);
            }
            // SAFETY: checked that next() will be Some, but also ensured
            // there is sufficent capacity.
            unsafe { self.push_within_capacity(iter.next().unwrap_unchecked()) }
        }
        Ok(())
    }

    /// Extends the collection by appending the elements from the slice. The
    /// capacity will not be increased.
    ///
    /// # Safety
    /// The caller needs to ensure there is enough capacity before calling
    /// this function (such as after a successful [Self::try_reserve]).
    #[inline]
    pub unsafe fn extend_from_slice_within_capacity(&mut self, data: &[T]) {
        let len = self.len();
        let additional = data.len();

        // SAFETY: valid pointer due to required reserved capacity.
        let begin = unsafe { self.ptr.cast::<T>().as_ptr().add(len) };

        // SAFETY: caller is required to ensure enough capacity, and since
        // we're adding to unused capacity, it's not possible for the input to
        // alias this space.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), begin, additional) };

        // SAFETY: If the safety conditions if this function are upheld, then
        // this cannot overflow nor cause len to exceed capacity.
        unsafe { self.set_len(len + additional) };
    }

    /// Tries to extend the collection by appending the elements from the
    /// slice. The capacity will not be increased.
    ///
    /// This is sometimes less efficient than
    /// [Self::extend_from_slice_within_capacity]`. The optimizer sometimes
    /// decides out that the capacity is adequate, but other times it doesn't.
    /// This can happen even if the failure branch is hinted with
    /// [core::hint::unreachable_unchecked] in the caller. This is why the
    /// unsafe version is provided as well.
    ///
    /// # Errors
    /// If there isn't enough capacity for all the elements of the slice, then
    /// none are pushed, and an error is returned.
    pub fn try_extend_from_slice_within_capacity(
        &mut self,
        values: &[T],
    ) -> Result<(), NeedsCapacity> {
        if self.needs_to_grow(self.len(), values.len()) {
            Err(NeedsCapacity {
                available: self.capacity(),
                needed: values.len(),
            })
        } else {
            // SAFETY: capacity was checked above.
            unsafe { self.extend_from_slice_within_capacity(values) };
            Ok(())
        }
    }

    /// Tries to extend the collection from the elements of the slice, growing
    /// the capacity if needed.
    ///
    /// # Errors
    /// If there is an error growing the capacity, then no elements are
    /// pushed, and an error is returned.
    #[inline]
    pub fn try_extend_from_slice(&mut self, values: &[T]) -> Result<(), TryReserveError> {
        self.try_reserve(values.len())?;
        unsafe { self.extend_from_slice_within_capacity(values) };
        Ok(())
    }

    /// Returns the number of elements the collection can hold without
    /// reallocating.
    #[inline]
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of elements the collection holds.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the collection contains no elements.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Shortens the collection by keeping the first `len` elements. This does
    /// not affect the capacity. If `len` is greater than or equal to the
    /// number of elements currently in the collection, then this method has
    /// no effect.
    #[inline]
    pub fn truncate(&mut self, len: usize) {
        if len >= self.len() {
            return;
        }

        // There's no need to drop anything, since T: Copy and therefore can't
        // have Drops to run.
        // SAFETY: shortening the length will not cause any safety issues.
        unsafe { self.set_len(len) };
    }

    /// Removes all elements from the vector without changing the capacity.
    #[inline]
    pub fn clear(&mut self) {
        self.truncate(0);
    }

    /// Sets the length of the collection.
    ///
    /// # Safety
    /// The len must be less than or equal to the capacity, and the first len
    /// elements of the collection must be properly initialized.
    #[inline(always)]
    pub unsafe fn set_len(&mut self, len: usize) {
        self.len = len;
    }

    /// Returns the remaining spare capacity as a slice of [mem::MaybeUninit].
    /// This can be used to fill data, and then call [Self::set_len].
    #[inline]
    pub const fn spare_capacity_mut(&mut self) -> &mut [mem::MaybeUninit<T>] {
        let len = self.len;
        unsafe {
            slice::from_raw_parts_mut(
                self.ptr.as_ptr().add(len).cast::<mem::MaybeUninit<T>>(),
                self.capacity - len,
            )
        }
    }

    /// Returns a `NonNull` pointer to the vector's buffer. It may be a
    /// dangling pointer if the vector hasn't allocated yet.
    #[inline]
    pub const fn as_non_null(&mut self) -> ptr::NonNull<T> {
        self.ptr
    }

    /// Creates a [FixedCapacityBuffer], using the storage of this vec.
    pub const fn as_fixed_capacity_buffer(&mut self) -> FixedCapacityBuffer<T> {
        FixedCapacityBuffer::from_virtual_vec(self)
    }
}

impl<T: Copy> VirtualVec<T> {
    #[inline(always)]
    fn needs_to_grow(&self, len: usize, additional: usize) -> bool {
        additional > self.capacity.wrapping_sub(len)
    }

    #[inline(always)]
    fn current_memory(&self) -> Option<(ptr::NonNull<u8>, Layout)> {
        if self.capacity > 0 {
            // SAFETY: we have an allocated chunk of memory for this layout
            // already, so it must be valid (or else it would have failed).
            let layout = unsafe { Layout::array::<T>(self.capacity).unwrap_unchecked() };
            Some((self.ptr.cast(), layout))
        } else {
            None
        }
    }

    #[inline(never)]
    #[cold]
    fn try_grow(&mut self, len: usize, additional: usize) -> Result<(), TryReserveError> {
        debug_assert!(additional > 0);
        let required_cap = len
            .checked_add(additional)
            .ok_or(TryReserveError::CapacityOverflow)?;
        // This guarantees exponential growth. The doubling cannot overflow
        // because `cap <= isize::MAX` and the type of `cap` is `usize`.
        let cap = cmp::max(self.capacity * 2, required_cap);
        // In the current implementation, the exact number doesn't matter, as
        // long as it's > 1. The virtual allocator is going to give us a much
        // larger capacity in practice.
        let cap = cmp::max(8, cap);
        let new_layout = Layout::array::<T>(cap);

        // `finish_grow` is non-generic over `T`. The Rust std library uses
        // this same technique.
        let ptr = finish_grow(new_layout, self.current_memory(), &VirtualAllocator)?;

        self.ptr = ptr.cast::<T>();

        // Use the full capacity returned by the allocator.
        self.capacity = ptr.len() / mem::size_of::<T>();
        Ok(())
    }
}

#[inline(always)]
fn alloc_guard(alloc_size: usize) -> Result<(), TryReserveError> {
    if usize::BITS < 64 && alloc_size > isize::MAX as usize {
        Err(TryReserveError::CapacityOverflow)
    } else {
        Ok(())
    }
}

#[inline(always)]
fn finish_grow(
    new_layout: Result<Layout, LayoutError>,
    current_memory: Option<(ptr::NonNull<u8>, Layout)>,
    alloc: &VirtualAllocator,
) -> Result<ptr::NonNull<[u8]>, TryReserveError> {
    let new_layout = new_layout.map_err(|_| TryReserveError::CapacityOverflow)?;

    alloc_guard(new_layout.size())?;

    let memory = if let Some((ptr, old_layout)) = current_memory {
        debug_assert_eq!(old_layout.align(), new_layout.align());
        if old_layout.align() != new_layout.align() {
            // SAFETY: The allocator checks for alignment equality.
            unsafe { core::hint::unreachable_unchecked() };
        }
        unsafe { alloc.grow(ptr, old_layout, new_layout) }
    } else {
        alloc.allocate_zeroed(new_layout)
    };

    memory.map_err(|_| TryReserveError::AllocError)
}

impl<T: Copy> Default for VirtualVec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy> Drop for VirtualVec<T> {
    fn drop(&mut self) {
        // Avoid dropping the dangling pointer for capacity=0.
        if self.capacity > 0 {
            // SAFETY: the object has been allocated, so its layout is valid.
            let layout = unsafe {
                Layout::from_size_align_unchecked(
                    self.capacity * mem::size_of::<T>(),
                    mem::align_of::<T>(),
                )
            };
            // SAFETY: todo
            unsafe { VirtualAllocator.deallocate(self.ptr.cast(), layout) };
        }
    }
}

impl<T: Copy> ops::Deref for VirtualVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len()) }
    }
}

impl<T: Copy> ops::DerefMut for VirtualVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len()) }
    }
}

#[cfg(feature = "std")]
use std::io;

#[cfg(feature = "std")]
impl From<TryReserveError> for io::Error {
    fn from(value: TryReserveError) -> Self {
        io::Error::new(io::ErrorKind::OutOfMemory, value)
    }
}

#[cfg(feature = "std")]
impl io::Write for VirtualVec<u8> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.try_extend_from_slice(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<T: Copy> NoGrowOps<T> for VirtualVec<T> {
    fn capacity(&self) -> usize {
        self.capacity()
    }

    unsafe fn set_len(&mut self, len: usize) {
        self.set_len(len)
    }
}

impl<T: Copy> MayGrowOps<T> for VirtualVec<T> {
    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.try_reserve(additional)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extend_within_capacity() {
        let mut vec = VirtualVec::new();
        let values = &[0, 1, 2, 3, 4, 5, 6, 7];
        vec.try_reserve(values.len()).unwrap();
        unsafe { vec.extend_from_slice_within_capacity(values) };
        assert_eq!(vec.len(), values.len());
        assert_eq!(values, &vec[..]);
    }

    #[test]
    fn test_push_within_capacity() {
        let mut vec = VirtualVec::new();
        let values = &[0, 1, 2, 3, 4, 5, 6, 7];
        vec.try_reserve(values.len()).unwrap();
        for value in values.iter().cloned() {
            unsafe { vec.push_within_capacity(value) };
        }
        assert_eq!(vec.len(), values.len());
        assert_eq!(values, &vec[..]);
    }

    #[test]
    fn test_grow() {
        let mut vec = VirtualVec::new();
        let values = &[0, 1, 2, 3, 4, 5, 6, 7];
        // This is an "allocate," not a "grow."
        vec.try_extend_from_slice(values).unwrap();

        for i in vec.len()..vec.capacity() {
            assert!(vec.len() < vec.capacity());
            unsafe { vec.push_within_capacity(i) };
            assert_eq!(i, vec[i]);
        }
        assert_eq!(vec.len(), vec.capacity());

        let pre_growth_len = vec.len();
        // Now we exercise the "grow" path for the first time.
        vec.try_extend_from_slice(values).unwrap();
        assert_eq!(vec.len(), pre_growth_len + values.len());

        for i in 0..pre_growth_len {
            assert_eq!(
                vec[i], i,
                "Expected index {i} to be {i}, received {}",
                vec[i]
            );
        }
        assert_eq!(&vec[pre_growth_len..], values);
        drop(vec);
    }
}
