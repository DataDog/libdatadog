// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free `Option<Box<T>>` with atomic take, backed by an `AtomicPtr`.

use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};

/// An `Option<Box<T>>` that supports lock-free atomic take.
///
/// # Storage
/// The value is stored as an `AtomicPtr<T>`, `None` being the null pointer. The pointer keeps
/// its provenance from `Box::into_raw`, so taking the value back is sound.
///
/// The stored box is dropped when the `AtomicOptionBox` itself is dropped, or when it is
/// taken/replaced.
pub struct AtomicOptionBox<T>(AtomicPtr<T>);

impl<T> AtomicOptionBox<T> {
    /// Atomically replace the stored value with `None` and return what was there.
    /// Returns `None` if the value was already taken.
    pub fn take(&self) -> Option<Box<T>> {
        let ptr = self.0.swap(ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: a non-null pointer here was produced by `Box::into_raw`, and the swap makes
        // this thread its sole owner, so reconstructing the box is sound.
        unsafe { NonNull::new(ptr).map(|ptr| Box::from_raw(ptr.as_ptr())) }
    }

    /// Borrow the current value without taking it.
    ///
    /// # Safety
    /// Must not be called concurrently with [`take`], and the returned reference must not
    /// outlive a subsequent [`take`] or drop of the value.
    pub unsafe fn as_ref(&self) -> Option<&T> {
        let ptr = self.0.load(Ordering::Acquire);
        // SAFETY: a non-null pointer is always a valid `Box<T>` (see `take`), and the caller
        // guarantees no concurrent mutation of the storage.
        unsafe { ptr.as_ref() }
    }
}

impl<T> From<Option<Box<T>>> for AtomicOptionBox<T> {
    fn from(val: Option<Box<T>>) -> Self {
        Self(AtomicPtr::new(
            val.map_or_else(ptr::null_mut, Box::into_raw),
        ))
    }
}

impl<T> Drop for AtomicOptionBox<T> {
    fn drop(&mut self) {
        let ptr = *self.0.get_mut();
        if !ptr.is_null() {
            // SAFETY: the pointer was produced by `Box::into_raw` and we have exclusive access.
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

// `AtomicPtr<T>` is `Send` and `Sync` when `T: Send` — same contract as `Mutex<Option<Box<T>>>`.
// Values of `T` are only ever moved in and out atomically, never shared (at least by safe
// functions), so `T: Send` suffices for both traits.
unsafe impl<T: Send> Send for AtomicOptionBox<T> {}
unsafe impl<T: Send> Sync for AtomicOptionBox<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_values_are_dropped_once() {
        let opt = AtomicOptionBox::from(Some(Box::new(String::from("a"))));
        // SAFETY: no concurrent access.
        assert_eq!(unsafe { opt.as_ref() }.map(String::as_str), Some("a"));
        assert_eq!(opt.take().as_deref().map(String::as_str), Some("a"));
        assert!(opt.take().is_none());
        // SAFETY: no concurrent access.
        assert!(unsafe { opt.as_ref() }.is_none());
    }

    #[test]
    fn value_is_dropped_when_container_is_dropped() {
        // Would leak without the `Drop` impl; detected by Miri.
        let opt = AtomicOptionBox::from(Some(Box::new(String::from("a"))));
        drop(opt);

        // `None` must not be freed.
        drop(AtomicOptionBox::<String>::from(None));
    }
}
