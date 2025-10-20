// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::error::FfiSafeErrorMessage;
use std::ffi::CStr;
use std::fmt::{Display, Formatter};
use std::ptr::null_mut;

/// Represents an object that should only be referred to by its handle.
/// Do not access its member for any reason, only use the C API functions on this struct.
#[repr(C)]
pub struct Handle<T> {
    // This may be null, but if not it will point to a valid <T>.
    inner: *mut T,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HandleError {
    OuterNullPtr,
    InnerNullPtr,
}

impl<T> Handle<T> {
    pub fn empty() -> Self {
        Self { inner: null_mut() }
    }

    /// Tries to create a new Handle from the provided value. Fails if memory
    /// cannot be allocated.
    pub fn try_new(t: T) -> Option<Handle<T>> {
        let uninit = allocator_api2::boxed::Box::<T>::try_new(t).ok()?;
        let inner = allocator_api2::boxed::Box::into_raw(uninit).cast();
        Some(Self { inner })
    }
}

/// # Safety
/// All cases use c-str literals to satisfy conditions.
unsafe impl FfiSafeErrorMessage for HandleError {
    fn as_ffi_str(&self) -> &'static CStr {
        match self {
            HandleError::OuterNullPtr => c"handle is null",
            HandleError::InnerNullPtr => {
                c"handle's interior pointer is null, indicates use-after-free"
            }
        }
    }
}

impl Display for HandleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_rust_str().fmt(f)
    }
}

impl core::error::Error for HandleError {}

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
        self.as_mut()
            .ok_or(HandleError::OuterNullPtr)?
            .to_inner_mut()
    }

    unsafe fn take(&mut self) -> Result<Box<T>, HandleError> {
        self.as_mut().ok_or(HandleError::OuterNullPtr)?.take()
    }
}

impl<T> ToInner<T> for Handle<T> {
    unsafe fn to_inner_mut(&mut self) -> Result<&mut T, HandleError> {
        self.inner.as_mut().ok_or(HandleError::InnerNullPtr)
    }

    unsafe fn take(&mut self) -> Result<Box<T>, HandleError> {
        // Leaving a null will help with double-free issues that can arise in C.
        // Of course, it's best to never get there in the first place!
        let raw = std::mem::replace(&mut self.inner, null_mut());
        if raw.is_null() {
            return Err(HandleError::InnerNullPtr);
        }
        Ok(Box::from_raw(raw))
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
        drop(unsafe { self.take() })
    }
}
