// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::slice;
use serde::ser::Error;
use serde::Serializer;
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::str::Utf8Error;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct Slice<'a, T: 'a> {
    /// Should be non-null and suitably aligned for the underlying type. It is
    /// allowed but not recommended for the pointer to be null when the len is
    /// zero.
    ptr: *const T,

    /// The number of elements (not bytes) that `.ptr` points to. Must be less
    /// than or equal to [isize::MAX].
    len: usize,
    _marker: PhantomData<&'a [T]>,
}

impl<'a, T: 'a> core::ops::Deref for Slice<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a, T: Debug> Debug for Slice<'a, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a, T: Eq> PartialEq<Self> for Slice<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<'a, T: Eq> Eq for Slice<'a, T> {}

/// Use to represent strings -- should be valid UTF-8.
pub type CharSlice<'a> = Slice<'a, c_char>;

/// Use to represent bytes -- does not need to be valid UTF-8.
pub type ByteSlice<'a> = Slice<'a, u8>;

/// This exists as an intrinsic, but it is private.
pub fn is_aligned_and_not_null<T>(ptr: *const T) -> bool {
    !ptr.is_null() && is_aligned(ptr)
}

#[inline]
fn is_aligned<T>(ptr: *const T) -> bool {
    debug_assert!(!ptr.is_null());
    ptr as usize % std::mem::align_of::<T>() == 0
}

pub trait AsBytes<'a> {
    fn as_bytes(&self) -> &'a [u8];

    #[inline]
    fn try_to_utf8(&self) -> Result<&'a str, Utf8Error> {
        std::str::from_utf8(self.as_bytes())
    }

    #[inline]
    fn to_utf8_lossy(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.as_bytes())
    }

    #[inline]
    /// # Safety
    /// Must only be used when the underlying data was already confirmed to be utf8.
    unsafe fn assume_utf8(&self) -> &'a str {
        std::str::from_utf8_unchecked(self.as_bytes())
    }
}

impl<'a> AsBytes<'a> for Slice<'a, u8> {
    fn as_bytes(&self) -> &'a [u8] {
        self.as_slice()
    }
}

impl<'a> AsBytes<'a> for Slice<'a, i8> {
    fn as_bytes(&self) -> &'a [u8] {
        // SAFETY: safe to convert *i8 to *u8 and then read it... I think.
        unsafe { Slice::from_raw_parts(self.ptr.cast(), self.len) }.as_slice()
    }
}

impl<'a, T: 'a> Slice<'a, T> {
    /// Creates a valid empty slice (len=0, ptr is non-null).
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            ptr: [].as_ptr(),
            len: 0,
            _marker: PhantomData,
        }
    }

    /// # Safety
    /// Uphold the same safety requirements as [std::str::from_raw_parts].
    /// However, it is allowed but not recommended to provide a null pointer
    /// when the len is 0.
    pub const unsafe fn from_raw_parts(ptr: *const T, len: usize) -> Self {
        Self {
            ptr,
            len,
            _marker: PhantomData,
        }
    }

    pub const fn new(slice: &[T]) -> Self {
        Self {
            ptr: slice.as_ptr(),
            len: slice.len(),
            _marker: PhantomData,
        }
    }

    pub fn as_slice(&self) -> &'a [T] {
        if !self.ptr.is_null() {
            // Crashing immediately is likely better than ignoring these.
            assert!(is_aligned(self.ptr));
            assert!(self.len <= isize::MAX as usize);
            unsafe { slice::from_raw_parts(self.ptr, self.len) }
        } else {
            // Crashing immediately is likely better than ignoring this.
            assert_eq!(self.len, 0);
            &[]
        }
    }

    pub fn into_slice(self) -> &'a [T] {
        self.as_slice()
    }
}

impl<'a, T> Default for Slice<'a, T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<'a, T> Hash for Slice<'a, T>
where
    Slice<'a, T>: AsBytes<'a>,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.as_bytes())
    }
}

impl<'a, T> serde::Serialize for Slice<'a, T>
where
    Slice<'a, T>: AsBytes<'a>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.try_to_utf8().map_err(Error::custom)?)
    }
}

impl<'a, T> Display for Slice<'a, T>
where
    Slice<'a, T>: AsBytes<'a>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.try_to_utf8().map_err(|_| std::fmt::Error)?)
    }
}

impl<'a, T: 'a> From<&'a [T]> for Slice<'a, T> {
    fn from(s: &'a [T]) -> Self {
        Slice::new(s)
    }
}

impl<'a, T> From<&'a Vec<T>> for Slice<'a, T> {
    fn from(value: &'a Vec<T>) -> Self {
        Slice::new(value)
    }
}

impl<'a> From<&'a str> for Slice<'a, c_char> {
    fn from(s: &'a str) -> Self {
        // SAFETY: Rust strings meet all the invariants required.
        unsafe { Slice::from_raw_parts(s.as_ptr().cast(), s.len()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_char;
    use std::ptr;

    #[test]
    fn slice_from_into_slice() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::from(raw);

        let converted: &[u8] = slice.into_slice();
        assert_eq!(converted, raw);
    }

    #[test]
    fn string_try_to_utf8() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::from(raw);

        let result = slice.try_to_utf8();
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[test]
    fn string_from_c_char() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = unsafe { Slice::from_raw_parts(raw.as_ptr() as *const c_char, raw.len()) };

        let result = slice.try_to_utf8();
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Foo(i64);

    #[test]
    fn slice_from_foo() {
        let raw = Foo(42);
        let ptr = &raw as *const _;
        let slice = unsafe { Slice::from_raw_parts(ptr, 1) };

        let expected: &[Foo] = &[raw];
        let actual: &[Foo] = slice.as_slice();

        assert_eq!(expected, actual)
    }

    #[test]
    fn test_iterator() {
        let slice: &[i32] = &[1, 2, 3];
        let slice = Slice::from(slice);

        let mut iter = slice.iter();

        assert_eq!(Some(&1), iter.next());
        assert_eq!(Some(&2), iter.next());
        assert_eq!(Some(&3), iter.next());
    }

    #[test]
    fn test_null_len0() {
        let null_len0: Slice<u8> = Slice {
            ptr: ptr::null(),
            len: 0,
            _marker: PhantomData,
        };
        assert_eq!(null_len0.as_slice(), &[]);
    }

    #[should_panic]
    #[test]
    fn test_null_panic() {
        let null_len0: Slice<u8> = Slice {
            ptr: ptr::null(),
            len: 1,
            _marker: PhantomData,
        };
        _ = null_len0.as_slice();
    }

    #[should_panic]
    #[test]
    fn test_long_panic() {
        let dangerous: Slice<u8> = Slice {
            ptr: ptr::NonNull::dangling().as_ptr(),
            len: isize::MAX as usize + 1,
            _marker: PhantomData,
        };
        _ = dangerous.as_slice();
    }
}
