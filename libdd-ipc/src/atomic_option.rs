// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lock-free `Option<T>` with atomic take, valid for any `T` where `Option<T>` has the size and
//! alignment of one of `u8`, `u16`, `u32` or `u64`.

use std::cell::UnsafeCell;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};

/// An `Option<T>` that supports lock-free atomic take.
///
/// # Constraints
/// `T` must have the size and and an alignement compatible with one of the available atomic
/// integers (typically `u8`, `u16`, `u32` and `u64`). More specifically, `size_of::<Option<T>>()`
/// must be exactly 1, 2, 4 or 8 bytes, and `align_of::<Option<T>>()` must be at least the alignment
/// of the atomic integer of that size. This is enforced at compile time through `const` assertions.
///
/// This holds for niche-optimised pointer-like types (`NonNull<T>`, `Box<T>`, `&T`, …) and for
/// small integers wrapped in a niche (`NonZeroU32`, …).
///
/// ```compile_fail
/// // `Option<[u8; 3]>` has size 4 but alignment 1: not suitably aligned for `AtomicU32`.
/// let _ = libdd_ipc::AtomicOption::<[u8; 3]>::from(None);
/// ```
///
/// ```compile_fail
/// // `Option<u64>` has size 16.
/// let _ = libdd_ipc::AtomicOption::<u64>::from(None);
/// ```
///
/// # Storage
/// The option is stored in a `UnsafeCell<Option<T>>`, giving it exactly the size and alignment
/// of `Option<T>` itself. Atomic operations reinterpret that storage as the atomic integer
/// `AtomicU<N>` with `N = 8 * size_of::<Option<T>>()`. This is valid thanks to the constraints
/// above.
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
    /// Check of the constraints documented on [`AtomicOption`] at compile time.
    const fn assert_layout() {
        let size = size_of::<Option<T>>();
        let align = align_of::<Option<T>>();

        let atomic_align = match size {
            1 => align_of::<AtomicU8>(),
            2 => align_of::<AtomicU16>(),
            4 => align_of::<AtomicU32>(),
            8 => align_of::<AtomicU64>(),
            // A panic in a `const fn` is compile-time, which is ok.
            #[allow(clippy::panic)]
            _ => panic!("AtomicOption requires the size of T to be either 1, 2, 4 or 8"),
        };

        assert!(
            align >= atomic_align,
            "AtomicOption requires that the aligment of T is equal to or greater than the alignement of the matching atomic"
        );
    }

    /// Encode `val` as a `u64`, transferring ownership into the bit representation.
    const fn encode(val: Option<T>) -> u64 {
        const { Self::assert_layout() };
        let mut bits = 0u64;
        // SAFETY: `assert_layout` guarantees `size_of::<Option<T>>() <= size_of::<u64>()`, so both
        // the source and the destination are valid for that many bytes. They don't overlap by core
        // guarantees of Rust (two distinct owned values). Ownership of `val` is moved into `bits`,
        // hence the `forget`.
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
        let ptr = self.0.get();
        // SAFETY: `assert_layout` (checked when constructing `self`) guarantees that
        // `size_of::<Option<T>>()` is one of the sizes below and that the storage is suitably
        // aligned for the corresponding atomic, so the cell is valid for atomic access of that
        // width. All concurrent accesses to the cell go through this function (apart from
        // `as_option`, whose contract forbids concurrency), so there is no mixed
        // atomic/non-atomic or mixed-size access.
        //
        // The `as` casts truncate `new_bits` to its low bytes, which is where `encode` put the
        // value, and zero-extend the result back, which `decode` ignores.
        unsafe {
            match size_of::<Option<T>>() {
                1 => AtomicU8::from_ptr(ptr.cast()).swap(new_bits as u8, Ordering::AcqRel) as u64,
                2 => AtomicU16::from_ptr(ptr.cast()).swap(new_bits as u16, Ordering::AcqRel) as u64,
                4 => AtomicU32::from_ptr(ptr.cast()).swap(new_bits as u32, Ordering::AcqRel) as u64,
                // Only 8 is left, per `assert_layout`.
                _ => AtomicU64::from_ptr(ptr.cast()).swap(new_bits, Ordering::AcqRel),
            }
        }
    }

    /// Reconstruct an `Option<T>` from its `u64` bit representation.
    ///
    /// # Safety
    /// `bits` must hold a valid `Option<T>` bit-pattern in its low
    /// `size_of::<Option<T>>()` bytes, as produced by a previous `encode`.
    const unsafe fn decode(bits: u64) -> Option<T> {
        // SAFETY: `assert_layout` (checked in `encode`, which produced `bits`) guarantees that
        // `size_of::<Option<T>>() <= size_of::<u64>()`, and the caller guarantees that these bytes
        // are a valid `Option<T>`.
        unsafe {
            let mut result = MaybeUninit::<Option<T>>::uninit();
            ptr::copy_nonoverlapping(
                ptr::from_ref(&bits).cast::<u8>(),
                result.as_mut_ptr().cast::<u8>(),
                size_of::<Option<T>>(),
            );
            result.assume_init()
        }
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
        // SAFETY: `old` holds a valid `Option<T>` bit-pattern.
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
        // SAFETY: the caller guarantees there is no concurrent write to the cell.
        unsafe { &*self.0.get() }
    }
}

impl<T> From<Option<T>> for AtomicOption<T> {
    fn from(val: Option<T>) -> Self {
        // We may allow 16 bytes once AtomicU128 becomes stable.
        const { Self::assert_layout() };
        Self(UnsafeCell::new(val))
    }
}

// SAFETY: `AtomicOption<T>` is `Send` and `Sync` when `T: Send` — same contract as
// `Mutex<Option<T>>`. Values of `T` are only ever moved in and out atomically, never shared (at
// least by safe functions).
unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send> Sync for AtomicOption<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::{NonZeroU8, NonZeroU16, NonZeroU32, NonZeroU64};

    fn roundtrip<T: Copy + PartialEq + std::fmt::Debug>(a: T, b: T) {
        let opt = AtomicOption::from(Some(a));
        assert_eq!(opt.replace(b), Some(a));
        assert_eq!(opt.take(), Some(b));
        assert_eq!(opt.take(), None);
        assert_eq!(opt.set(Some(a)), None);
        // SAFETY: no concurrent access.
        assert_eq!(unsafe { opt.as_option() }, &Some(a));
    }

    #[test]
    fn supported_sizes() {
        roundtrip(NonZeroU8::MIN, NonZeroU8::MAX);
        roundtrip(NonZeroU16::MIN, NonZeroU16::MAX);
        roundtrip(NonZeroU32::MIN, NonZeroU32::MAX);
        roundtrip(NonZeroU64::MIN, NonZeroU64::MAX);
        // Discriminant-based option (`None` isn't all zeros).
        roundtrip(false, true);
    }

    #[test]
    fn owned_values_are_dropped_once() {
        let opt = AtomicOption::from(Some(Box::new(String::from("a"))));
        let old = opt.replace(Box::new(String::from("b")));
        assert_eq!(old.as_deref().map(String::as_str), Some("a"));
        assert_eq!(opt.take().as_deref().map(String::as_str), Some("b"));
        assert!(opt.take().is_none());
    }
}
