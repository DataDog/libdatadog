// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This file contains the internal structures used by `ObservationMap` and
//! `TimestampedObservationMap`. See the comment on mod.rs for more explanation.

use std::mem;

/// This represents the length of a TrimmedObservation.  This is private to
/// this module, which means that only the `*Map` types can create and use
/// these.  This helps to ensure that the lengths given when we rehydrate a
/// slice are the same as when we trimmed it.
#[repr(transparent)]
#[derive(Copy, Clone, Default, Debug)]
pub(super) struct ObservationLength(usize);

impl ObservationLength {
    pub fn eq(&self, other: usize) -> bool {
        self.0 == other
    }

    pub fn assert_eq(&self, other: usize) {
        assert_eq!(self.0, other, "Expected observation lengths to be the same");
    }

    pub const fn new(obs_len: usize) -> Self {
        Self(obs_len)
    }
}

/// This represents a `Vec<i64>` associated with a sample
/// Since these vectors are all of the same length, there is no need to store
/// `len` and `capacity` fields over and over again for each sample.
/// Instead, just keep the pointer, and recreate the slice as needed.
///
/// # Safety
/// This panics if you attempt to create an Observation with a data vector
/// of the wrong length.
#[repr(transparent)]
pub(super) struct TrimmedObservation {
    data: *mut i64,
}

/// # Safety
/// Since [TrimmedObservation] is essentially Box<[i64]> that's been shrunk
/// down in size with no other semantic changes, and that type is [Send], then
/// so is [TrimmedObservation].
unsafe impl Send for TrimmedObservation {}

impl TrimmedObservation {
    /// Safety: the ObservationLength must have come from the same profile as the Observation
    pub unsafe fn as_mut_slice(&mut self, len: ObservationLength) -> &mut [i64] {
        unsafe { std::slice::from_raw_parts_mut(self.data, len.0) }
    }

    /// Consumes self, ensuring that the memory behind it is dropped.
    /// It is an error to drop a TrimmedObservation without consuming it first.
    /// Safety: the ObservationLength must have come from the same profile as the Observation
    pub unsafe fn consume(self, len: ObservationLength) {
        drop(self.into_boxed_slice(len));
    }

    /// Converts a `Vec<i64>` representing sample observations
    /// into a more memory efficient `Observation`
    /// # Safety
    /// This panics if you attempt to create an Observation with a data vector
    /// of the wrong length.
    pub fn new(v: Vec<i64>, len: ObservationLength) -> Self {
        len.assert_eq(v.len());

        // First, convert the vector into a boxed slice.
        // This shrinks any excess capacity on the vec.
        // At this point, the memory is now owned by the box.
        // https://doc.rust-lang.org/std/vec/struct.Vec.html#method.into_boxed_slice
        let b = v.into_boxed_slice();
        // Get the fat pointer representing the slice out of the box.
        // At this point, we now own the memory
        // https://doc.rust-lang.org/std/boxed/struct.Box.html#method.into_raw
        let p = Box::into_raw(b);
        // Get the pointer to just the data part of the slice, throwing away
        // the length metadata.
        // At this point, we are now responsible for tracking the length
        // ourselves.
        let data = p as *mut i64;
        Self { data }
    }

    /// Safety: the ObservationLength must have come from the same profile as the Observation
    unsafe fn into_boxed_slice(mut self, len: ObservationLength) -> Box<[i64]> {
        unsafe {
            let s: &mut [i64] = std::slice::from_raw_parts_mut(
                mem::replace(&mut self.data, std::ptr::null_mut()),
                len.0,
            );
            Box::from_raw(s)
        }
    }

    /// Safety: the ObservationLength must have come from the same profile as the Observation
    pub(super) unsafe fn into_vec(mut self, len: ObservationLength) -> Vec<i64> {
        unsafe {
            // We built this from a vec.  Put it back together again.
            Vec::from_raw_parts(
                mem::replace(&mut self.data, std::ptr::null_mut()),
                len.0,
                len.0,
            )
        }
    }
}

impl Drop for TrimmedObservation {
    /// Dropping a TrimmedObservation that still owns data is an error.
    /// By the time this is called, the owner of the `TrimmedObservation` should
    /// have consumed the memory using `consume()`.
    fn drop(&mut self) {
        assert_eq!(
            self.data,
            std::ptr::null_mut(),
            "Dropped TrimmedObservation that still owned data."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_mut_test() {
        let v = vec![1, 2];
        let o = ObservationLength::new(2);
        let mut t = TrimmedObservation::new(v, o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &vec![1, 2]);
            t.as_mut_slice(o).iter_mut().for_each(|v| *v *= 2);
            assert_eq!(t.as_mut_slice(o), &vec![2, 4]);
            t.consume(o);
        }
    }

    #[test]
    fn drop_after_emptying_test() {
        let v = vec![1, 2];
        let o = ObservationLength::new(2);
        let t = TrimmedObservation::new(v, o);
        unsafe {
            t.consume(o);
        }
    }

    #[test]
    #[should_panic]
    // This test has an explicit memory leak, and shows that we panic if that
    // happens
    #[cfg_attr(miri, ignore)]
    fn drop_owned_data_panics_test() {
        let v = vec![1, 2];
        let o = ObservationLength::new(2);
        let _t = TrimmedObservation::new(v, o);
    }

    #[test]
    fn into_boxed_slice_test() {
        let v = vec![1, 2];
        let o = ObservationLength::new(2);
        let mut t = TrimmedObservation::new(v, o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &vec![1, 2]);
            let b = t.into_boxed_slice(o);
            assert_eq!(*b, vec![1, 2]);
        }
    }

    #[test]
    fn into_vec_test() {
        let v = vec![1, 2];
        let o = ObservationLength::new(2);
        let mut t = TrimmedObservation::new(v, o);
        unsafe {
            assert_eq!(t.as_mut_slice(o), &vec![1, 2]);
            let b = t.into_vec(o);
            assert_eq!(*b, vec![1, 2]);
        }
    }
}
