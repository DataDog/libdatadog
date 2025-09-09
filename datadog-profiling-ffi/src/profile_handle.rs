// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A profile handle is similar to the ddcommon_ffi::Handle, but its repr
//! is transparent, and it does not implement Drop. It also does not offer APIs
//! which panic, such as From because it has to Box, which may fail.
//! This is an experiment to see how it works comparatively.
//!
//! To dispose of it, call [`ProfileHandle::take`] and drop the box.

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use datadog_profiling::profiles::ProfileError;
use ddcommon::error::FfiSafeErrorMessage;
use std::ffi::CStr;
use std::fmt;
use std::ptr::NonNull;

// Represents an object that should only be referred to by its handle.
#[repr(transparent)]
pub struct ProfileHandle<T> {
    /// A null pointer is a valid but almost useless handle as all operations
    /// will error or return None. It's still good for initialization and
    /// detecting some misuse. The pointer is only valid until it's dropped
    /// through any handle to the same resource. If a handle is copied, then
    /// it may be invalid even if it's non-null!
    ptr: *mut T,
}

/// Note that this type is Copy because it's an FFI type; we cannot stop C code
/// from copying it, so we are reflecting that fact. It is not recommended to
/// copy a handle.
impl<T> Copy for ProfileHandle<T> {}

impl<T> Default for ProfileHandle<T> {
    fn default() -> Self {
        Self { ptr: std::ptr::null_mut() }
    }
}

impl<T> fmt::Debug for ProfileHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileHandle")
            .field("ptr", &NonNull::new(self.ptr))
            .finish()
    }
}

impl<T> Clone for ProfileHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct EmptyHandleError;

/// # Safety
///
/// Uses c-str literal to ensure valid UTF-8 and null termination.
unsafe impl ddcommon::error::FfiSafeErrorMessage for EmptyHandleError {
    fn as_ffi_str(&self) -> &'static CStr {
        c"handle used with an interior null pointer"
    }
}

impl fmt::Display for EmptyHandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_rust_str().fmt(f)
    }
}

impl core::error::Error for EmptyHandleError {}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct AllocHandleError(AllocError);

impl From<AllocError> for AllocHandleError {
    fn from(e: AllocError) -> Self {
        Self(e)
    }
}

impl AllocHandleError {
    /// Returns the error message as a static reference to a CStr, which means
    /// it is null terminated.
    /// This is also guaranteed to valid UTF-8.
    pub const fn message() -> &'static CStr {
        c"memory allocation failed: profile handle couldn't be made"
    }

    pub const fn message_str() -> &'static str {
        // str::from_utf8_unchecked isn't stable until 1.87, so duplicate it.
        "memory allocation failed: profile handle couldn't be made"
    }
}

impl fmt::Display for AllocHandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Self::message_str().fmt(f)
    }
}

impl core::error::Error for AllocHandleError {}

impl From<EmptyHandleError> for ProfileError {
    fn from(err: EmptyHandleError) -> ProfileError {
        ProfileError::other(err.as_rust_str())
    }
}

impl From<AllocHandleError> for ProfileError {
    fn from(_: AllocHandleError) -> ProfileError {
        ProfileError::other(AllocHandleError::message_str())
    }
}

impl<T> ProfileHandle<T> {
    /// Tries to heap-allocate the provided value and provide a handle to it.
    /// Fails if the allocator fails.
    pub fn try_new(t: T) -> Result<Self, AllocHandleError> {
        let ptr = Box::into_raw(Box::try_new(t)?).cast();
        Ok(Self { ptr })
    }

    /// Returns the underlying boxed value if the handle is not empty.
    ///
    /// # Safety
    ///
    /// This function should not be called from different handles to the same
    /// underlying resource. Example of issue:
    ///  1. A handle is copied.
    ///  2. Take is called on the original handle.
    ///  3. Take is called on the copied handle, which isn't aware of the take
    ///     from step 2, and so you get two Box<T> to the same value.
    ///
    /// Taking from the same handle multiple times is supported and safe.
    pub unsafe fn take(&mut self) -> Option<Box<T>> {
        (!self.ptr.is_null()).then(|| unsafe { Box::from_raw(self.ptr.cast()) })
    }

    /// Tries to return a reference to the underlying value.
    ///
    /// # Safety
    ///
    ///  1. The handle's underlying resource must still be alive.
    ///  2. No mutable references to the same underlying resource must exist.
    ///     This includes references from other handles to the same underlying
    ///     resource.
    pub unsafe fn as_inner(&self) -> Result<&T, EmptyHandleError> {
        unsafe { self.ptr.cast::<T>().as_ref() }.ok_or(EmptyHandleError)
    }

    /// Tries to return a mutable reference to the underlying value.
    ///
    /// # Safety
    ///
    ///  1. The handle's underlying resource must still be alive.
    ///  2. No references to the same underlying resource must exist,
    ///     even if it comes from a different handle to the same resource.
    pub unsafe fn as_inner_mut(&mut self) -> Result<&mut T, EmptyHandleError> {
        unsafe { self.ptr.cast::<T>().as_mut() }.ok_or(EmptyHandleError)
    }
}

impl<T> From<Box<T>> for ProfileHandle<T> {
    fn from(ptr: Box<T>) -> Self {
        let ptr = Box::into_raw(ptr).cast();
        Self { ptr }
    }
}
