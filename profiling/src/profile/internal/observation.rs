// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::cell::RefCell;

thread_local! {
    static LENGTH: RefCell<Option<usize>> = RefCell::new( None);
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
pub struct Observation {
    data: *mut i64,
}

impl From<Vec<i64>> for Observation {
    /// Converts a `Vec<i64>` representing sample observations
    /// into a more memory efficient `Observation`
    /// # Safety
    /// This panics if you attempt to create an Observation with a data vector
    /// of the wrong length.
    fn from(v: Vec<i64>) -> Self {
        Self::new(v)
    }
}

impl std::convert::AsRef<[i64]> for Observation {
    fn as_ref(&self) -> &[i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts(self.data, len)
        }
    }
}

impl std::convert::AsMut<[i64]> for Observation {
    fn as_mut(&mut self) -> &mut [i64] {
        unsafe {
            let len: usize = Self::len().expect("LENGTH to exist by the time we use it");
            std::slice::from_raw_parts_mut(self.data, len)
        }
    }
}

impl Observation {
    pub fn iter(&self) -> core::slice::Iter<'_, i64> {
        self.as_ref().iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, i64> {
        self.as_mut().iter_mut()
    }

    pub fn len() -> Option<usize> {
        LENGTH.with(|len| *len.borrow())
    }

    /// Converts a `Vec<i64>` representing sample observations
    /// into a more memory efficient `Observation`
    /// # Safety
    /// This panics if you attempt to create an Observation with a data vector
    /// of the wrong length.
    fn new(v: Vec<i64>) -> Self {
        if let Some(len) = Self::len() {
            assert_eq!(len, v.len(), "Sample observation was the wrong length");
        } else {
            LENGTH.with(|len| *len.borrow_mut() = Some(v.len()));
        }
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
        // ourselves, which we do in the LENGTH static.
        let data = p as *mut i64;
        Self { data }
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        unsafe {
            // To drop we need to recreate the original Box, and drop that
            // https://doc.rust-lang.org/std/boxed/struct.Box.html#method.into_raw
            let r = self.as_mut() as *mut [i64];
            let b = Box::from_raw(r);
            // TODO: is this necessary, or will drop be automatically called when
            // `b` goes out of scope?
            std::mem::drop(b)
        }
    }
}
