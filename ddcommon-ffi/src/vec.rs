// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

extern crate alloc;

use crate::slice::Slice;
use core::ops::Deref;
use std::io::Write;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ptr::NonNull;

/// Holds the raw parts of a Rust Vec; it should only be created from Rust,
/// never from C.
// The names ptr and len were chosen to minimize conversion from a previous
// Buffer type which this has replaced to become more general.
#[repr(C)]
#[derive(Debug)]
pub struct Vec<T: Sized> {
    ptr: *const T,
    len: usize,
    capacity: usize,
    _marker: PhantomData<T>,
}

unsafe impl<T: Send> Send for Vec<T> {}

unsafe impl<T: Sync> Sync for Vec<T> {}

impl<T: PartialEq> PartialEq for Vec<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Eq> Eq for Vec<T> {}

impl<T> Drop for Vec<T> {
    fn drop(&mut self) {
        let vec =
            unsafe { alloc::vec::Vec::from_raw_parts(self.ptr as *mut T, self.len, self.capacity) };
        drop(vec)
    }
}

impl<T> From<Vec<T>> for alloc::vec::Vec<T> {
    fn from(vec: Vec<T>) -> Self {
        let v = ManuallyDrop::new(vec);
        unsafe { alloc::vec::Vec::from_raw_parts(v.ptr as *mut T, v.len, v.capacity) }
    }
}

impl<T> From<alloc::vec::Vec<T>> for Vec<T> {
    fn from(vec: alloc::vec::Vec<T>) -> Self {
        let mut v = ManuallyDrop::new(vec);
        Self {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            capacity: v.capacity(),
            _marker: PhantomData,
        }
    }
}

impl From<anyhow::Error> for Vec<u8> {
    fn from(err: anyhow::Error) -> Self {
        let mut vec = vec![];
        write!(vec, "{err}").expect("write to vec to always succeed");
        Self::from(vec)
    }
}

impl<'a, T> IntoIterator for &'a Vec<T> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().iter()
    }
}

impl<T> Vec<T> {
    fn replace(&mut self, mut vec: ManuallyDrop<std::vec::Vec<T>>) {
        self.ptr = vec.as_mut_ptr();
        self.len = vec.len();
        self.capacity = vec.capacity();
    }

    pub fn push(&mut self, value: T) {
        // todo: I'm never sure when to propagate unsafe upwards
        let mut vec = ManuallyDrop::new(unsafe {
            alloc::vec::Vec::from_raw_parts(self.ptr as *mut T, self.len, self.capacity)
        });
        vec.push(value);
        self.replace(vec);
    }

    pub fn as_slice(&self) -> Slice<T> {
        unsafe { Slice::from_raw_parts(self.ptr, self.len) }
    }
    
    pub const fn new() -> Self {
        Vec {
            ptr: NonNull::dangling().as_ptr(),
            len: 0,
            capacity: 0,
            _marker: PhantomData,
        }
    } 
}

impl<T> Deref for Vec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice().as_slice()
    }
}

impl<T> Default for Vec<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default() {
        let vec: Vec<u8> = Vec::default();
        assert_eq!(vec.len, 0);
        assert_eq!(vec.capacity, 0);
    }

    #[test]
    fn test_from() {
        let vec = vec![0];

        let mut ffi_vec: Vec<u8> = Vec::from(vec);
        ffi_vec.push(1);
        assert_eq!(ffi_vec.len(), 2);
        assert!(ffi_vec.capacity >= 2);
    }

    #[test]
    fn test_as_slice() {
        let mut ffi_vec: Vec<u8> = Vec::default();
        ffi_vec.push(1);
        ffi_vec.push(2);
        assert_eq!(ffi_vec.len(), 2);
        assert!(ffi_vec.capacity >= 2);

        let slice = ffi_vec.deref();
        let [first, second]: [_; 2] = slice.try_into().expect("slice to have 2 items");
        assert_eq!(first, 1);
        assert_eq!(second, 2);
    }

    #[test]
    fn test_iter() {
        let vec = vec![0, 2, 4, 6];
        let ffi_vec: Vec<u8> = Vec::from(vec.clone());

        for (a, b) in vec.iter().zip(ffi_vec.into_iter()) {
            assert_eq!(a, b)
        }
    }
}
