// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Extension trait for `Mutex` to provide a method that acquires a lock, panicking if the lock is
/// poisoned.
///
/// This helper function is intended to be used to avoid having to add many
/// `#[allow(clippy::unwrap_used)]` annotations if there are a lot of usages of `Mutex`.
///
/// # Arguments
///
/// * `self` - A reference to the `Mutex` to lock.
///
/// # Returns
///
/// A `MutexGuard` that provides access to the locked data.
///
/// # Panics
///
/// This function will panic if the `Mutex` is poisoned.
///
/// # Examples
///
/// ```
/// use libdd_common::MutexExt;
/// use std::sync::{Arc, Mutex};
///
/// let data = Arc::new(Mutex::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.lock_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.lock_or_panic(), 6);
/// ```
pub trait MutexExt<T> {
    fn lock_or_panic(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    #[inline(always)]
    #[track_caller]
    fn lock_or_panic(&self) -> MutexGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.lock().unwrap()
    }
}

/// Extension trait for `RwLock` to provide methods that acquire read/write locks, panicking if
/// the lock is poisoned.
///
/// Mirrors [`MutexExt`] for `RwLock` so callers avoid `#[allow(clippy::unwrap_used)]` at each
/// lock site.
///
/// # Examples
///
/// ```
/// use libdd_common::RwLockExt;
/// use std::sync::{Arc, RwLock};
///
/// let data = Arc::new(RwLock::new(5));
/// let data_clone = Arc::clone(&data);
///
/// std::thread::spawn(move || {
///     let mut num = data_clone.write_or_panic();
///     *num += 1;
/// })
/// .join()
/// .expect("Thread panicked");
///
/// assert_eq!(*data.read_or_panic(), 6);
/// ```
pub trait RwLockExt<T> {
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T>;
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    #[inline(always)]
    #[track_caller]
    fn read_or_panic(&self) -> RwLockReadGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.read().unwrap()
    }

    #[inline(always)]
    #[track_caller]
    fn write_or_panic(&self) -> RwLockWriteGuard<'_, T> {
        #[allow(clippy::unwrap_used)]
        self.write().unwrap()
    }
}
