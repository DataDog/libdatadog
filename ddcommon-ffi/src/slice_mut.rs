// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::AsBytes;
use crate::Slice;
use core::{marker, mem, ptr, slice};
use serde::ser::Error;
use serde::Serializer;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::os::raw::c_char;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct MutSlice<'a, T: 'a> {
    /// Should be non-null and suitably aligned for the underlying type. It is
    /// allowed but not recommended for the pointer to be null when the len is
    /// zero.
    ptr: Option<ptr::NonNull<T>>,

    /// The number of elements (not bytes) that `.ptr` points to. Must be less
    /// than or equal to [isize::MAX].
    len: usize,
    _marker: marker::PhantomData<&'a mut [T]>,
}

impl<'a, T: 'a> From<MutSlice<'a, T>> for Slice<'a, T> {
    fn from(value: MutSlice<'a, T>) -> Slice<'a, T> {
        let ptr = value.ptr.unwrap_or(ptr::NonNull::dangling()).as_ptr();
        unsafe { Slice::from_raw_parts(ptr, value.len) }
    }
}

impl<T: Debug> Debug for MutSlice<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'a, T: 'a> core::ops::Deref for MutSlice<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

/// Use to represent strings -- should be valid UTF-8.
pub type CharMutSlice<'a> = MutSlice<'a, c_char>;

/// Use to represent bytes -- does not need to be valid UTF-8.
pub type ByteMutSlice<'a> = MutSlice<'a, u8>;

#[inline]
fn is_aligned<T>(ptr: ptr::NonNull<T>) -> bool {
    ptr.as_ptr() as usize % mem::align_of::<T>() == 0
}

impl<'a> AsBytes<'a> for MutSlice<'a, u8> {
    fn try_as_bytes(&self) -> Option<&'a [u8]> {
        Slice::from(*self).try_as_bytes()
    }
}

impl<'a, T: 'a> MutSlice<'a, T> {
    /// Creates a valid empty slice (len=0, ptr is non-null).
    // TODO, this can be const once MSRV >= 1.85
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ptr: Some(ptr::NonNull::dangling()),
            len: 0,
            _marker: marker::PhantomData,
        }
    }

    /// # Safety
    /// Uphold the same safety requirements as [str::from_raw_parts].
    /// However, it is allowed but not recommended to provide a null pointer
    /// when the len is 0.
    // TODO, this can be const once MSRV >= 1.85
    pub unsafe fn from_raw_parts(ptr: *mut T, len: usize) -> Self {
        Self {
            ptr: ptr::NonNull::new(ptr),
            len,
            _marker: marker::PhantomData,
        }
    }

    // TODO, this can be const once MSRV >= 1.85
    pub fn new(slice: &mut [T]) -> Self {
        Self {
            ptr: ptr::NonNull::new(slice.as_mut_ptr()),
            len: slice.len(),
            _marker: marker::PhantomData,
        }
    }

    pub fn as_mut_slice(&mut self) -> &'a mut [T] {
        if let Some(ptr) = self.ptr {
            // Crashing immediately is likely better than ignoring these.
            assert!(is_aligned(ptr));
            assert!(self.len <= isize::MAX as usize);
            unsafe { slice::from_raw_parts_mut(ptr.as_ptr(), self.len) }
        } else {
            // Crashing immediately is likely better than ignoring this.
            assert_eq!(self.len, 0);
            &mut []
        }
    }

    pub fn as_slice(&self) -> &'a [T] {
        if let Some(ptr) = self.ptr {
            // Crashing immediately is likely better than ignoring these.
            assert!(is_aligned(ptr));
            assert!(self.len <= isize::MAX as usize);
            unsafe { slice::from_raw_parts(ptr.as_ptr(), self.len) }
        } else {
            // Crashing immediately is likely better than ignoring this.
            assert_eq!(self.len, 0);
            &[]
        }
    }

    /// Tries to convert the FFI slice into a standard slice.
    ///
    /// # Errors
    ///
    ///  1. Fails if `self.ptr` is null and `self.len` is not zero.
    ///  2. Fails if `self.ptr` is not null and is unaligned.
    ///  3. Fails if `self.len` is larger than [`isize::MAX`].
    ///
    /// # Safety
    ///
    /// Although it checks for some errors, there are some that cannot be
    /// checked but must be upheld:
    ///  1. If `self.len` is more than 0, then `self.ptr` must be valid for `self.len` writes, which
    ///     will not drop any existing values. It does not need to be valid for reads, which allows
    ///     for uninitialized slices.
    pub unsafe fn try_as_uninit(&self) -> Option<&'a mut [mem::MaybeUninit<T>]> {
        let ptr = self.ptr?;
        (ptr.is_aligned() && self.len <= isize::MAX as usize)
            .then(|| unsafe { slice::from_raw_parts_mut(ptr.as_ptr().cast(), self.len) })
    }

    pub fn into_slice(self) -> &'a [T] {
        self.as_slice()
    }

    pub fn into_mut_slice(mut self) -> &'a mut [T] {
        self.as_mut_slice()
    }
}

impl<T> Default for MutSlice<'_, T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<'a, T> Hash for MutSlice<'a, T>
where
    MutSlice<'a, T>: AsBytes<'a>,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.as_bytes())
    }
}

impl<'a, T> serde::Serialize for MutSlice<'a, T>
where
    MutSlice<'a, T>: AsBytes<'a>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.try_to_utf8().map_err(Error::custom)?)
    }
}

impl<'a, T> Display for MutSlice<'a, T>
where
    MutSlice<'a, T>: AsBytes<'a>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.try_to_utf8().map_err(|_| std::fmt::Error)?)
    }
}

impl<'a, T: 'a> From<&'a mut [T]> for MutSlice<'a, T> {
    fn from(s: &'a mut [T]) -> Self {
        MutSlice::new(s)
    }
}

impl<'a, T> From<&'a mut Vec<T>> for MutSlice<'a, T> {
    fn from(value: &'a mut Vec<T>) -> Self {
        MutSlice::new(value)
    }
}

impl<'a> From<&'a mut str> for MutSlice<'a, c_char> {
    fn from(s: &'a mut str) -> Self {
        // SAFETY: Rust strings meet all the invariants required.
        unsafe { MutSlice::from_raw_parts(s.as_mut_ptr().cast(), s.len()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct Foo(i64);

    #[test]
    fn slice_from_foo() {
        let mut raw = Foo(42);
        let ptr = &mut raw as *mut _;
        let mut slice = unsafe { MutSlice::from_raw_parts(ptr, 1) };

        let expected: &[Foo] = &[raw];
        let actual: &[Foo] = slice.as_mut_slice();

        assert_eq!(expected, actual)
    }

    #[test]
    fn test_iterator() {
        let slice: &mut [i32] = &mut [1, 2, 3];
        let slice = MutSlice::from(slice);

        let mut iter = slice.iter();

        assert_eq!(Some(&1), iter.next());
        assert_eq!(Some(&2), iter.next());
        assert_eq!(Some(&3), iter.next());
    }

    #[test]
    fn test_null_len0() {
        let mut null_len0: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 0,
            _marker: marker::PhantomData,
        };
        assert_eq!(null_len0.as_mut_slice(), &[]);
    }

    #[should_panic]
    #[test]
    fn test_null_panic() {
        let mut null_len0: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 1,
            _marker: marker::PhantomData,
        };
        _ = null_len0.as_mut_slice();
    }

    #[should_panic]
    #[test]
    fn test_long_panic() {
        let mut dangerous: MutSlice<u8> = MutSlice {
            ptr: Some(ptr::NonNull::dangling()),
            len: isize::MAX as usize + 1,
            _marker: marker::PhantomData,
        };
        _ = dangerous.as_mut_slice();
    }
}
