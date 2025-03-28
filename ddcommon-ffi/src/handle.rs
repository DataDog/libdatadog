// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ptr::null_mut;

use anyhow::Context;

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
    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut T>;
    /// # Safety
    /// The Handle must hold a valid `inner` [return OK(inner)], or null [returns Error].
    unsafe fn take(&mut self) -> anyhow::Result<Box<T>>;
}

impl<T> ToInner<T> for *mut Handle<T> {
    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut T> {
        self.as_mut().context("Null pointer")?.to_inner_mut()
    }

    unsafe fn take(&mut self) -> anyhow::Result<Box<T>> {
        self.as_mut().context("Null pointer")?.take()
    }
}

impl<T> ToInner<T> for Handle<T> {
    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut T> {
        self.inner
            .as_mut()
            .context("inner pointer was null, indicates use after free")
    }

    unsafe fn take(&mut self) -> anyhow::Result<Box<T>> {
        // Leaving a null will help with double-free issues that can arise in C.
        // Of course, it's best to never get there in the first place!
        let raw = std::mem::replace(&mut self.inner, std::ptr::null_mut());
        anyhow::ensure!(
            !raw.is_null(),
            "inner pointer was null, indicates use after free"
        );
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
