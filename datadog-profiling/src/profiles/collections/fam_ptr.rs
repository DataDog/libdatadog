// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::marker;
use core::ptr::{slice_from_raw_parts, slice_from_raw_parts_mut, NonNull};

/// A [`FamPtr`] is a pointer to a struct with a flexible-array-member of type
/// `T`. This exists only because fat pointers do not have stable ABIs,
/// although they kind of do because the tricks we use to re-construct it is
/// documented as correct language behavior.
/// TODO: find the documentation for this and link it.
#[repr(C)]
#[derive(Debug)]
pub struct FamPtr<T> {
    /// Pointer to the beginning of the object.
    ptr: NonNull<u8>,
    /// The offset from the beginning of the object to the start of the FAM.
    offset: usize,
    /// The "length" of the FAM, which is typically the capacity and not the
    /// length of usable Ts (which may be smaller).
    cap: usize,
    _marker: marker::PhantomData<*mut T>,
}

// This can't be derived for some reason, not fully sure why.
impl<T> Clone for FamPtr<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr,
            offset: self.offset,
            cap: self.cap,
            _marker: Default::default(),
        }
    }
}

impl<T> Copy for FamPtr<T> {}

impl<T> FamPtr<T> {
    /// Creates a new [`FamPtr`] using the provided pointer to the object,
    /// offset to the flexible-array-member in bytes from the start of the
    /// object, and the capacity of the flexible-array-member.
    ///
    /// # Safety
    ///
    /// The values provided must match the actual layout of the object
    /// represented by [`FamPtr`]. For now, this probably means that the object
    /// represented by the pointer must be repr(C), but in the future
    /// `Layout::for_value_raw` and some other functions may allow this for
    /// repr(Rust) types.
    pub unsafe fn new(ptr: NonNull<u8>, offset: usize, cap: usize) -> Self {
        Self {
            ptr,
            offset,
            cap,
            _marker: marker::PhantomData,
        }
    }

    /// Converts the [`FamPtr`] to a wide pointer to T. This is not a
    /// reference because it's not guaranteed that all Ts are initialized.
    /// Also, there's no guarantee it's
    pub fn array_ptr(&self) -> *mut [T] {
        // SAFETY: required by constructors
        let ptr = unsafe { self.ptr.as_ptr().add(self.offset) };
        slice_from_raw_parts_mut(ptr.cast(), self.cap)
    }

    /// Returns the pointer to the object.
    pub fn object_ptr(&self) -> NonNull<u8> {
        self.ptr
    }

    /// Returns the "length" of the FAM, which is typically the capacity and
    /// not the length of usable Ts (which may be smaller).
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Returns a wide pointer to the base of the object. The caller can then
    /// cast this into the dynamically sized type which this [`FamPtr`]
    /// represents.
    pub fn wide_object_ptr(&self) -> *const [()] {
        slice_from_raw_parts(self.ptr.as_ptr(), self.capacity()) as *const [()]
    }
}
