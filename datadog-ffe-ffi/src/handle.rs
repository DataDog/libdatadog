// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// An opaque handle for a resource. The inner fields must not be dereferenced.
///
/// This is similar to `ddcommon_ffi::Handle` but only allows shared access to internal resource, so
/// it's safe to share between thread or access concurrently (if the underlying type is).
///
/// # Ownership
///
/// `Handle::free()` must be called exactly once on any created Handle. Failure to do that will
/// result in a memory leak.
#[repr(transparent)]
pub struct Handle<T> {
    inner: *mut T,
}

// SAFETY: the box pointer is safe to move across threads as long as the underlying type is Send.
unsafe impl<T: Send> Send for Handle<T> {}
// SAFETY: we only hand off shared refences, so it's Sync as long as underlying type is Sync.
unsafe impl<T: Sync> Sync for Handle<T> {}

impl<T> Handle<T> {
    /// Create a new handle to `T`.
    ///
    /// # Ownership
    ///
    /// This moves `value` to heap.
    ///
    /// `Handle::free()` must be called exactly once on any created Handle. Failure to do that will
    /// result in a memory leak.
    pub(crate) fn new(value: T) -> Handle<T> {
        Handle {
            inner: Box::into_raw(Box::new(value)),
        }
    }

    /// Get a reference to inner value.
    ///
    /// # Safety
    /// - `self` must be a valid handle for `T`.
    #[allow(clippy::expect_used)]
    pub(crate) unsafe fn as_ref(&self) -> &T {
        // SAFETY: the caller must ensure that self is valid
        unsafe { self.inner.as_ref() }.expect("detected use after free")
    }

    /// Free this handle. This and all other copies of the handle become invalid after freeing.
    ///
    /// # Safety
    /// - `this` must be a valid pointer to valid handle for `T`.
    pub(crate) unsafe fn free(this: *mut Self) {
        if this.is_null() {
            return;
        }

        // SAFETY: the caller must ensure that the pointer is valid.
        let ptr = std::mem::replace(&mut (unsafe { &mut *this }).inner, std::ptr::null_mut());
        if ptr.is_null() {
            // We try to detect double-free but it's not fool-proof. The C side might have copied
            // the handle.
            debug_assert!(false, "detected double-free");
            return;
        }

        // SAFETY: the original value was created by Box::into_raw().
        let value = unsafe { Box::from_raw(ptr) };

        drop(value);
    }
}

impl<T> From<T> for Handle<T> {
    fn from(value: T) -> Self {
        Handle::new(value)
    }
}
