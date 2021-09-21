// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::convert::{TryFrom, TryInto};
use std::marker::PhantomData;
use std::ops::Sub;
use std::os::raw::c_char;
use std::str::Utf8Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeZone, Utc};
use libc::size_t;

mod exporter;
mod profiles;

/// Represents time since the Unix Epoch in seconds plus nanoseconds.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Timespec {
    pub seconds: i64,
    pub nanoseconds: u32,
}

impl From<Timespec> for DateTime<Utc> {
    fn from(value: Timespec) -> Self {
        Utc.timestamp(value.seconds, value.nanoseconds)
    }
}

impl TryFrom<SystemTime> for Timespec {
    type Error = Box<dyn std::error::Error>;

    fn try_from(value: SystemTime) -> Result<Self, Self::Error> {
        let mut duration = value.duration_since(UNIX_EPOCH)?;
        let seconds: i64 = duration.as_secs().try_into()?;
        duration = duration.sub(Duration::from_secs(seconds as u64));
        let nanoseconds: u32 = duration.as_nanos().try_into()?;
        Ok(Self {
            seconds,
            nanoseconds,
        })
    }
}

/// Buffer holds the raw parts of a Rust Vec; it should only be created from
/// Rust, never from C.
#[repr(C)]
pub struct Buffer {
    ptr: *const u8,
    len: size_t,
    capacity: size_t,
}

impl Buffer {
    pub fn from_vec(vec: Vec<u8>) -> Self {
        let buffer = Self {
            ptr: vec.as_ptr(),
            len: vec.len(),
            capacity: vec.capacity(),
        };
        std::mem::forget(vec);
        buffer
    }

    /// # Safety
    /// This operation is only safe if the buffer was created from using one of
    /// the associated methods on `Buffer`.
    pub unsafe fn as_slice(&self) -> &[u8] {
        std::slice::from_raw_parts(self.ptr, self.len)
    }

    /// # Safety
    /// This operation is only safe if the buffer was created from using one of
    /// the associated methods on `Buffer`.
    pub unsafe fn into_vec(self) -> Vec<u8> {
        let ptr = self.ptr as *mut u8;
        let vec = Vec::from_raw_parts(ptr, self.len, self.capacity);
        std::mem::forget(self);
        vec
    }

    /// # Safety
    /// This operation is only safe if the buffer was created from using one of
    /// the associated methods on `Buffer`.
    pub unsafe fn reset(&mut self) {
        *self = Self::from_vec(Vec::new());
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        let vec: Vec<u8> =
            unsafe { Vec::from_raw_parts(self.ptr as *mut u8, self.len, self.capacity) };
        std::mem::drop(vec)
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Slice<'a, T> {
    pub ptr: *const T,
    pub len: size_t,
    phantom: PhantomData<&'a [T]>,
}

/// This exists as an intrinsic, but it is private.
pub fn is_aligned_and_not_null<T>(ptr: *const T) -> bool {
    !ptr.is_null() && ptr as usize % std::mem::align_of::<T>() == 0
}

impl<'a, T> Slice<'a, T> {
    pub fn new(ptr: *const T, len: size_t) -> Self {
        Self {
            ptr,
            len,
            phantom: Default::default(),
        }
    }

    /// # Safety
    /// The Slice's ptr must point to contiguous storage of at least `self.len`
    /// elements. If it is not suitably aligned, then it will return an empty slice.
    pub unsafe fn into_slice(self) -> &'a [T] {
        if self.is_empty() {
            return &[];
        }
        std::slice::from_raw_parts(self.ptr, self.len)
    }

    pub fn len(&self) -> usize {
        if is_aligned_and_not_null(self.ptr) {
            self.len
        } else {
            0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0 || !is_aligned_and_not_null(self.ptr)
    }
}

impl<'a, T> Default for Slice<'a, T> {
    fn default() -> Self {
        /* The docs on std::slice::from_raw_parts indicate the pointer should be
         * non-null and suitably aligned for T even for zero-length slices.
         * Using a few tests, I wasn't actually able to create any harm with a
         * null pointer; after all it shouldn't get de-referenced and such, but
         * nonetheless we follow the documentation and use NonNull::dangling(),
         * which it suggests.
         * Since Slice's can be made from C, check for null and unaligned
         * pointers in associated functions to defend against this.
         */
        Self {
            ptr: std::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            phantom: Default::default(),
        }
    }
}

impl<'a, T> From<&'a [T]> for Slice<'a, T> {
    fn from(s: &'a [T]) -> Self {
        Slice::new(s.as_ptr() as *const T, s.len())
    }
}

impl<'a, T> From<&Vec<T>> for Slice<'a, T> {
    fn from(value: &Vec<T>) -> Self {
        let ptr = value.as_ptr();
        let len = value.len();
        Slice::new(ptr, len)
    }
}

impl<'a> From<&'a str> for Slice<'a, c_char> {
    fn from(s: &'a str) -> Self {
        Slice::new(s.as_ptr() as *const c_char, s.len())
    }
}

impl<'a, T> From<Slice<'a, T>> for &'a [T] {
    fn from(value: Slice<'a, T>) -> &'a [T] {
        unsafe { std::slice::from_raw_parts(value.ptr, value.len) }
    }
}

impl<'a> TryFrom<Slice<'a, u8>> for &'a str {
    type Error = Utf8Error;

    fn try_from(value: Slice<'a, u8>) -> Result<Self, Self::Error> {
        let slice = unsafe { value.into_slice() };
        std::str::from_utf8(slice)
    }
}

impl<'a> TryFrom<Slice<'a, c_char>> for &'a str {
    type Error = Utf8Error;

    fn try_from(slice: Slice<'a, c_char>) -> Result<Self, Self::Error> {
        // delegate to Slice<u8> implementation
        let bytes = Slice::new(slice.ptr as *const u8, slice.len);
        bytes.try_into()
    }
}

impl<'a> From<Slice<'a, c_char>> for Option<&'a str> {
    fn from(value: Slice<'a, c_char>) -> Self {
        match value.try_into() {
            Ok(str) => Some(str),
            Err(_) => None,
        }
    }
}

#[cfg(test)]
mod test {
    use std::convert::TryInto;
    use std::os::raw::c_char;
    use std::str::Utf8Error;

    use crate::Slice;

    #[test]
    fn slice_from_u8() {
        let raw = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::new(raw.as_ptr(), raw.len());

        let converted: &[u8] = slice.into();
        assert_eq!(converted, raw);
    }

    #[test]
    fn string_try_from_u8() {
        let raw = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::new(raw.as_ptr(), raw.len());

        let result: Result<&str, Utf8Error> = slice.try_into();
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[test]
    fn string_try_from_c_char() {
        let raw = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::new(raw.as_ptr() as *const c_char, raw.len());

        let result: Result<&str, Utf8Error> = slice.try_into();
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Foo(i64);

    #[test]
    fn slice_from_foo() {
        let raw = Foo(42);
        let ptr = &raw as *const Foo;
        let slice = Slice::new(ptr, 1);

        let expected = &[raw];
        let actual: &[Foo] = slice.into();

        assert_eq!(expected, actual)
    }

    #[test]
    fn slice_from_null() {
        let ptr: *const usize = std::ptr::null();
        let expected: &[usize] = &[];
        let actual: &[usize] = Slice::new(ptr, 0).into();
        assert_eq!(expected, actual);
    }
}
