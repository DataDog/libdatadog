// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

//! This file contains the internal structures used by `ObservationMap` and `TimestampedObservationMap`.
//! See the comment on mod.rs for more explanation.

use std::mem;

/// This represents the length of a TrimmedObservation.  This is private to
/// this module, which means that only the `*Map` types can create and use
/// these.  This helps to ensure that the lengths given when we rehydrate a
/// slice are the same as when we trimmed it.
#[repr(transparent)]
#[derive(Copy, Clone)]
pub(super) struct ObservationLength(usize);

impl ObservationLength {
    pub fn assert_eq(&self, other: usize) {
        assert_eq!(self.0, other, "Expected observation lengths to be the same");
    }

    pub fn new(obs_len: usize) -> Self {
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

impl TrimmedObservation {
    /// Safety: the ObservationLength must have come from the same profile as the Observation
    pub fn as_mut(&mut self, len: ObservationLength) -> &mut [i64] {
        unsafe { std::slice::from_raw_parts_mut(self.data, len.0) }
    }

    /// Safety: the ObservationLength must have come from the same profile as the Observation
    pub fn as_ref(&self, len: ObservationLength) -> &[i64] {
        unsafe { std::slice::from_raw_parts(self.data, len.0) }
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
    pub fn into_boxed_slice(mut self, len: ObservationLength) -> Box<[i64]> {
        unsafe {
            let s: &mut [i64] = std::slice::from_raw_parts_mut(
                mem::replace(&mut self.data, std::ptr::null_mut()),
                len.0,
            );
            Box::from_raw(s)
        }
    }
}

impl Drop for TrimmedObservation {
    /// Dropping a TrimmedObservation that still owns data is an error.
    /// By the time this is called, the owner of the `TrimmedObservation` should
    /// have extracted the memory using `into_boxed_slice`.
    fn drop(&mut self) {
        assert_eq!(
            self.data,
            std::ptr::null_mut(),
            "Dropped TrimmedObservation that still owned data."
        );
    }
}

