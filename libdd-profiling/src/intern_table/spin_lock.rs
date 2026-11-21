// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The writer lock between threads of one process.

use core::hint;
use core::sync::atomic::{AtomicU32, Ordering};

/// A spin lock on its own 128-byte line. It lives in the process-private
/// part of a table, so each process (including a forked child, which gets a
/// copy) has its own.
#[repr(C, align(128))]
pub struct SpinLock {
    held: AtomicU32,
}

impl SpinLock {
    pub const fn new() -> Self {
        Self {
            held: AtomicU32::new(0),
        }
    }

    /// Tries once, then up to six more times after doubling backoffs: at most
    /// 1 + 2 + 4 + 8 + 16 + 32 = 63 spin hints. This bounds the work, not the
    /// time, since the OS may deschedule the holder.
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_>> {
        if self.try_once() {
            return Some(SpinLockGuard(self));
        }
        for round in 0..6 {
            for _ in 0..1_u32 << round {
                hint::spin_loop();
            }
            // Test before the read-modify-write, to avoid taking the line
            // exclusively while the lock is held.
            if self.held.load(Ordering::Relaxed) == 0 && self.try_once() {
                return Some(SpinLockGuard(self));
            }
        }
        None
    }

    /// Waits for the lock. Only fork preparation waits; table operations use
    /// [`Self::try_lock`].
    pub fn lock(&self) -> SpinLockGuard<'_> {
        loop {
            if self.try_once() {
                return SpinLockGuard(self);
            }
            while self.held.load(Ordering::Relaxed) != 0 {
                hint::spin_loop();
            }
        }
    }

    /// Releases the lock without a guard, for a lock whose guard was
    /// forgotten (a lock held across `fork` through FFI).
    ///
    /// # Safety
    ///
    /// The lock is held, and no guard for it will release it.
    pub unsafe fn force_unlock(&self) {
        self.held.store(0, Ordering::Release);
    }

    fn try_once(&self) -> bool {
        self.held
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

/// Releases the lock on drop.
pub struct SpinLockGuard<'g>(&'g SpinLock);

impl Drop for SpinLockGuard<'_> {
    fn drop(&mut self) {
        self.0.held.store(0, Ordering::Release);
    }
}
