// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::EmptyHandleError;
use datadog_profiling2::profiles::collections::Arc;
use datadog_profiling2::profiles::ProfileError;
use std::ptr::{null_mut, NonNull};

/// Opaque FFI handle to an `Arc<T>`'s inner `T`.
///
/// Safety rules for implementors/callers:
/// - Do not create multiple owning `Arc<T>`s from the same raw pointer.
/// - Always restore the original `Arc` with `into_raw` after any `from_raw`.
/// - Use `as_inner()` to validate non-null before performing raw round-trips.
#[repr(transparent)]
#[derive(Debug)]
pub struct ArcHandle2<T>(*mut T);

impl<T> Copy for ArcHandle2<T> {}
impl<T> Clone for ArcHandle2<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Default for ArcHandle2<T> {
    fn default() -> Self {
        Self(null_mut())
    }
}

impl<T> ArcHandle2<T> {
    /// Constructs a new handle by allocating an `Arc<T>` and returning its
    /// inner pointer as a handle. Returns OutOfMemory on allocation failure.
    pub fn new(value: T) -> Result<Self, ProfileError> {
        let arc = Arc::try_new(value)?;
        let ptr = Arc::into_raw(arc).as_ptr();
        Ok(Self(ptr))
    }

    #[inline]
    pub fn as_inner(&self) -> Result<&T, EmptyHandleError> {
        unsafe { self.0.as_ref() }.ok_or(EmptyHandleError)
    }

    /// Tries to clone the resource this handle points to, and returns a new
    /// handle to it.
    pub fn try_clone(&self) -> Result<Self, ProfileError> {
        let nn = NonNull::new(self.0).ok_or(EmptyHandleError)?;
        // SAFETY: ArcHandle uses a pointer to T as its repr, and as long as
        // callers have upheld safety requirements elsewhere, including the
        // FFI, then there will be a valid object with refcount > 0.
        unsafe { Arc::try_increment_count(nn.as_ptr())? };
        Ok(Self(self.0))
    }

    /// Drops the resource that this handle refers to. It will remain alive if
    /// there are other handles to the resource which were created by
    /// successful calls to try_clone. This handle will now be empty and
    /// operations on it will fail.
    pub fn drop_resource(&mut self) {
        // pointers aren't default until Rust 1.88.
        let ptr = core::mem::replace(&mut self.0, null_mut());
        if let Some(nn) = NonNull::new(ptr) {
            drop(unsafe { Arc::from_raw(nn) });
        }
    }
}
