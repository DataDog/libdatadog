// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::u31::u31;
use core::ops::{Deref, DerefMut};
use datadog_alloc::buffer::{MayGrowOps, NoGrowOps, TryReserveError};

/// A wrapper around a type which implements `MayGrowOps`, and enforces that
/// the whole message cannot 2 GiB or greater.
pub struct Buffer<'a, T: MayGrowOps<u8>> {
    buf: &'a mut T,
    cap: usize,
}

impl<'a, T: MayGrowOps<u8>> Buffer<'a, T> {
    pub fn try_from(buf: &'a mut T) -> Result<Self, TryReserveError> {
        let cap = buf.capacity().min(i32::MAX as usize);
        if cap > i32::MAX as usize {
            Err(TryReserveError::CapacityOverflow)
        } else {
            Ok(Buffer { buf, cap })
        }
    }

    #[inline]
    pub fn len_u31(&self) -> u31 {
        // SAFETY: in the constructor and in try_reserve, we ensure this will
        // not overflow i32::MAX.
        unsafe { u31::new_unchecked(self.buf.len() as u32) }
    }
}

impl<T: MayGrowOps<u8>> Deref for Buffer<'_, T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buf.deref()
    }
}

impl<T: MayGrowOps<u8>> DerefMut for Buffer<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buf.deref_mut()
    }
}

impl<T: MayGrowOps<u8>> NoGrowOps<u8> for Buffer<'_, T> {
    fn capacity(&self) -> usize {
        self.cap
    }

    unsafe fn set_len(&mut self, len: usize) {
        self.buf.set_len(len);
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.buf.as_mut_ptr()
    }
}

impl<T: MayGrowOps<u8>> MayGrowOps<u8> for Buffer<'_, T> {
    fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
        self.buf.try_reserve(additional)?;
        self.cap = self.buf.capacity().min(i32::MAX as usize);
        if additional <= self.remaining_capacity() {
            Ok(())
        } else {
            Err(TryReserveError::CapacityOverflow)
        }
    }
}
