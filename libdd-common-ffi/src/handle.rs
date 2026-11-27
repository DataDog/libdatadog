// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "std")]

use core::ptr::null_mut;

use libdd_common::error::FfiSafeErrorMessage;

/// An allocation-free error returned when a handle cannot be accessed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandleError {
    /// The pointer to the handle was null.
    NullPointer,
    /// The handle had already been consumed or dropped.
    Empty,
}

impl HandleError {
    fn message(self) -> &'static core::ffi::CStr {
        match self {
            Self::NullPointer => c"handle pointer was null",
            Self::Empty => c"handle was empty",
        }
    }
}

impl core::fmt::Display for HandleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_rust_str())
    }
}

impl core::error::Error for HandleError {}

// SAFETY: both messages are UTF-8 c-str literals with static storage.
unsafe impl FfiSafeErrorMessage for HandleError {
    fn as_ffi_str(&self) -> &'static core::ffi::CStr {
        self.message()
    }
}

impl From<HandleError> for crate::Error {
    fn from(value: HandleError) -> Self {
        Self::from_display(value)
    }
}

/// Represents an object that should only be referred to by its handle.
/// Do not access its member for any reason, only use the C API functions on this struct.
#[repr(C)]
pub struct Handle<T> {
    // This may be null, but if not it will point to a valid <T>.
    inner: *mut T,
}

impl<T> Handle<T> {
    pub fn empty() -> Self {
        Self { inner: null_mut() }
    }
}

pub trait ToInner<T> {
    /// # Safety
    /// The Handle must hold a valid `inner` which has been allocated and not freed.
    unsafe fn to_inner_mut(&mut self) -> Result<&mut T, HandleError>;
    /// # Safety
    /// The Handle must hold a valid `inner` [return OK(inner)], or null [returns Error].
    unsafe fn take(&mut self) -> Result<Box<T>, HandleError>;
}

impl<T> ToInner<T> for *mut Handle<T> {
    unsafe fn to_inner_mut(&mut self) -> Result<&mut T, HandleError> {
        unsafe {
            self.as_mut()
                .ok_or(HandleError::NullPointer)?
                .to_inner_mut()
        }
    }

    unsafe fn take(&mut self) -> Result<Box<T>, HandleError> {
        unsafe { self.as_mut().ok_or(HandleError::NullPointer)?.take() }
    }
}

impl<T> ToInner<T> for Handle<T> {
    unsafe fn to_inner_mut(&mut self) -> Result<&mut T, HandleError> {
        unsafe { self.inner.as_mut().ok_or(HandleError::Empty) }
    }

    unsafe fn take(&mut self) -> Result<Box<T>, HandleError> {
        unsafe {
            // Leaving a null will help with double-free issues that can arise in C.
            // Of course, it's best to never get there in the first place!
            let raw = core::mem::replace(&mut self.inner, core::ptr::null_mut());
            if raw.is_null() {
                return Err(HandleError::Empty);
            }
            Ok(Box::from_raw(raw))
        }
    }
}

impl<T> From<T> for Handle<T> {
    fn from(value: T) -> Self {
        Self {
            inner: Box::into_raw(Box::new(value)),
        }
    }
}

impl<T> Drop for Handle<T> {
    fn drop(&mut self) {
        let raw = core::mem::replace(&mut self.inner, core::ptr::null_mut());
        if !raw.is_null() {
            // SAFETY: non-null inner pointers come from Box::into_raw, and replacing the pointer
            // above ensures this allocation is reclaimed at most once.
            drop(unsafe { Box::from_raw(raw) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    #[test]
    fn empty_handle_access_and_drop_do_not_allocate() {
        assert_no_alloc(|| unsafe {
            let mut null_handle = core::ptr::null_mut::<Handle<u8>>();
            assert_eq!(
                null_handle.to_inner_mut().unwrap_err(),
                HandleError::NullPointer
            );
            assert_eq!(null_handle.take().unwrap_err(), HandleError::NullPointer);

            let mut empty = Handle::<u8>::empty();
            assert_eq!(empty.to_inner_mut().unwrap_err(), HandleError::Empty);
            assert_eq!(empty.take().unwrap_err(), HandleError::Empty);
            drop(empty);
        });
    }
}
