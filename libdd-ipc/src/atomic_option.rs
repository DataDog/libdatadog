// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free `Option<T>` with atomic take, valid for any `T` where
//! `size_of::<Option<T>>() <= 8`.

use std::cell::UnsafeCell;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

/// An `Option<T>` that supports lock-free atomic take.
///
/// # Constraints
/// `size_of::<Option<T>>()` must be `<= 8`.  Enforced by a `debug_assert` in
/// `From<Option<T>>`).  This holds for niche-optimised types (`NonNull<T>`,
/// `Box<T>`, …) and for any `Option<T>` that fits in a single machine word.
///
/// # Storage
/// The option is stored in a `UnsafeCell<Option<T>>`, giving it exactly the size
/// and alignment of `Option<T>` itself.  `take()` picks the narrowest atomic that
/// covers `size_of::<Option<T>>()` bytes — `AtomicU8` for 1-byte options up to
/// `AtomicU64` for 5–8 byte options.  The atomic cast is valid because
/// `align_of::<AtomicUN>() == align_of::<uN>() <= align_of::<Option<T>>()`.
///
/// # None sentinel
/// The "none" bit-pattern is computed by value (`Option::<T>::None`) rather than
/// assumed to be zero, so the implementation is correct for both niche-optimised
/// types and discriminant-based options.
///
/// `UnsafeCell` provides the interior-mutability aliasing permission required by
/// Rust's memory model when mutating through a shared reference.
pub struct AtomicOption<T>(UnsafeCell<Option<T>>);

impl<T> AtomicOption<T> {
    /// Encode `val` as a `u64`, transferring ownership into the bit representation.
    const fn encode(val: Option<T>) -> u64 {
        let mut bits = 0u64;
        unsafe {
            ptr::copy_nonoverlapping(
                ptr::from_ref(&val).cast::<u8>(),
                ptr::from_mut(&mut bits).cast::<u8>(),
                size_of::<Option<T>>(),
            );
            mem::forget(val);
        }
        bits
    }

    /// Atomically swap the storage with `new_bits`, returning the old bits.
    #[inline]
    fn atomic_swap(&self, new_bits: u64) -> u64 {
        unsafe {
            let ptr = self.0.get();
            match size_of::<Option<T>>() {
                1 => (*(ptr as *const AtomicU8)).swap(new_bits as u8, Ordering::AcqRel) as u64,
                2 => (*(ptr as *const AtomicU16)).swap(new_bits as u16, Ordering::AcqRel) as u64,
                3 | 4 => {
                    (*(ptr as *const AtomicU32)).swap(new_bits as u32, Ordering::AcqRel) as u64
                }
                _ => (*(ptr as *const AtomicU64)).swap(new_bits, Ordering::AcqRel),
            }
        }
    }

    /// Reconstruct an `Option<T>` from its `u64` bit representation.
    ///
    /// # Safety
    /// `bits` must hold a valid `Option<T>` bit-pattern in its low
    /// `size_of::<Option<T>>()` bytes, as produced by a previous `encode`.
    const unsafe fn decode(bits: u64) -> Option<T> {
        let mut result = MaybeUninit::<Option<T>>::uninit();
        ptr::copy_nonoverlapping(
            ptr::from_ref(&bits).cast::<u8>(),
            result.as_mut_ptr().cast::<u8>(),
            size_of::<Option<T>>(),
        );
        result.assume_init()
    }

    /// Atomically replace the stored value with `None` and return what was there.
    /// Returns `None` if the value was already taken.
    pub fn take(&self) -> Option<T> {
        let old = self.atomic_swap(Self::encode(None));
        // SAFETY: `old` holds a valid `Option<T>` bit-pattern.
        unsafe { Self::decode(old) }
    }

    /// Atomically store `val`, dropping any previous value.
    pub fn set(&self, val: Option<T>) -> Option<T> {
        let old = self.atomic_swap(Self::encode(val));
        unsafe { Self::decode(old) }
    }

    /// Atomically store `Some(val)`, returning the previous value.
    pub fn replace(&self, val: T) -> Option<T> {
        self.set(Some(val))
    }

    /// Borrow the current value without taking it.
    ///
    /// # Safety
    /// Must not be called concurrently with [`take`], [`set`], or [`replace`].
    pub unsafe fn as_option(&self) -> &Option<T> {
        &*self.0.get()
    }
}

impl<T> From<Option<T>> for AtomicOption<T> {
    fn from(val: Option<T>) -> Self {
        // we may raise this to 16 once AtomicU128 becomes stable
        debug_assert!(
            size_of::<Option<T>>() <= size_of::<u64>(),
            "AtomicOption requires size_of::<Option<T>>() <= 8, got {}",
            size_of::<Option<T>>()
        );
        Self(UnsafeCell::new(val))
    }
}

// `AtomicOption<T>` is `Send`/`Sync` when `T: Send` — same contract as `Mutex<Option<T>>`.
unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}
