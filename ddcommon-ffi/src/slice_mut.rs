// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::slice::{AsBytes, SliceConversionError};
use core::slice;
use serde::ser::Error;
use serde::Serializer;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::ptr::NonNull;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct MutSlice<'a, T: 'a> {
    /// Should be non-null and suitably aligned for the underlying type. It is
    /// allowed but not recommended for the pointer to be null when the len is
    /// zero.
    ptr: Option<NonNull<T>>,

    /// The number of elements (not bytes) that `.ptr` points to. Must be less
    /// than or equal to [isize::MAX].
    len: usize,
    _marker: PhantomData<&'a mut [T]>,
}

impl<'a, T: 'a> core::ops::Deref for MutSlice<'a, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Debug> Debug for MutSlice<'_, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

/// Use to represent strings -- should be valid UTF-8.
pub type CharMutSlice<'a> = MutSlice<'a, c_char>;

/// Use to represent bytes -- does not need to be valid UTF-8.
pub type ByteMutSlice<'a> = MutSlice<'a, u8>;

impl<'a> AsBytes<'a> for MutSlice<'a, u8> {
    fn as_bytes(&self) -> &'a [u8] {
        self.as_slice()
    }

    fn try_as_bytes(&self) -> Result<&'a [u8], SliceConversionError> {
        self.try_as_slice()
    }
}

impl<'a, T: 'a> MutSlice<'a, T> {
    /// Creates a valid empty slice (len=0, ptr is non-null).
    // TODO, this can be const once MSRV >= 1.85
    #[must_use]
    pub fn empty() -> Self {
        Self {
            ptr: Some(NonNull::dangling()),
            len: 0,
            _marker: PhantomData,
        }
    }

    /// # Safety
    /// Uphold the same safety requirements as [std::str::from_raw_parts].
    /// However, it is allowed but not recommended to provide a null pointer
    /// when the len is 0.
    // TODO, this can be const once MSRV >= 1.85
    pub unsafe fn from_raw_parts(ptr: *mut T, len: usize) -> Self {
        Self {
            ptr: NonNull::new(ptr),
            len,
            _marker: PhantomData,
        }
    }

    // TODO, this can be const once MSRV >= 1.85
    pub fn new(slice: &mut [T]) -> Self {
        Self {
            ptr: NonNull::new(slice.as_mut_ptr()),
            len: slice.len(),
            _marker: PhantomData,
        }
    }

    pub fn as_mut_slice(&mut self) -> &'a mut [T] {
        if let Some(ptr) = self.ptr {
            // Crashing immediately is likely better than ignoring these.
            assert!(ptr.is_aligned());
            assert!(self.len <= isize::MAX as usize);
            unsafe { slice::from_raw_parts_mut(ptr.as_ptr(), self.len) }
        } else {
            // Crashing immediately is likely better than ignoring this.
            assert_eq!(self.len, 0);
            &mut []
        }
    }

    pub fn as_slice(&self) -> &'a [T] {
        #[allow(clippy::expect_used)]
        self.try_as_slice()
            .expect("ffi MutSlice failed to convert to a Rust slice")
    }

    /// Tries to convert the FFI slice as a Rust slice of bytes.
    ///
    /// # Errors
    ///
    ///  - Returns [`SliceConversionError::NullPointer`] if the slice has a null pointer and a
    ///    length other than zero. If pointer is null and length is zero, then return `Ok(&[])`
    ///    instead.
    ///  - Returns [`SliceConversionError::MisalignedPointer`] if the pointer is non-null and is not
    ///    aligned correctly for the type.
    ///  - Returns [`SliceConversionError::LargeLength`] if the length of the slice exceeds
    ///    [`isize::MAX`].
    pub fn try_as_slice(&self) -> Result<&'a [T], SliceConversionError> {
        if let Some(ptr) = self.ptr {
            if self.len > isize::MAX as usize {
                Err(SliceConversionError::LargeLength)
            } else if !ptr.is_aligned() {
                Err(SliceConversionError::MisalignedPointer)
            } else {
                Ok(unsafe { slice::from_raw_parts(ptr.as_ptr(), self.len) })
            }
        } else if self.len != 0 {
            Err(SliceConversionError::NullPointer)
        } else {
            Ok(&[])
        }
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
    use std::ptr;

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
            _marker: PhantomData,
        };
        assert_eq!(null_len0.as_mut_slice(), &[]);
    }

    #[should_panic]
    #[test]
    fn test_null_panic() {
        let mut null_len0: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 1,
            _marker: PhantomData,
        };
        _ = null_len0.as_mut_slice();
    }

    #[should_panic]
    #[test]
    fn test_long_panic() {
        let mut dangerous: MutSlice<u8> = MutSlice {
            ptr: Some(ptr::NonNull::dangling()),
            len: isize::MAX as usize + 1,
            _marker: PhantomData,
        };
        _ = dangerous.as_mut_slice();
    }

    #[test]
    fn test_try_as_slice_success() {
        let mut data = vec![1u8, 2, 3, 4, 5];
        let slice = MutSlice::from(data.as_mut_slice());

        let result = slice.try_as_slice();
        assert_eq!(result.unwrap(), &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_try_as_slice_null_zero_len() {
        let null_zero_len: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 0,
            _marker: PhantomData,
        };

        let result = null_zero_len.try_as_slice();
        assert_eq!(result.unwrap(), &[]);
    }

    #[test]
    fn test_try_as_slice_null_nonzero_len() {
        let null_nonzero: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 5,
            _marker: PhantomData,
        };

        let result = null_nonzero.try_as_slice();
        assert!(matches!(
            result.unwrap_err(),
            SliceConversionError::NullPointer
        ));
    }

    #[test]
    fn test_try_as_slice_large_length() {
        let large_len: MutSlice<u8> = MutSlice {
            ptr: Some(ptr::NonNull::dangling()),
            len: isize::MAX as usize + 1,
            _marker: PhantomData,
        };

        let result = large_len.try_as_slice();
        assert!(matches!(
            result.unwrap_err(),
            SliceConversionError::LargeLength
        ));
    }

    #[test]
    fn test_try_as_slice_misaligned_pointer() {
        // Create a misaligned pointer for u64 by using a properly aligned
        // array, then offset by 1 byte.
        let mut data = [0u64; 2];
        let base_ptr = data.as_mut_ptr().cast::<u8>();
        let misaligned_ptr = unsafe { base_ptr.add(1).cast::<u64>() };

        // Verify the pointer is actually misaligned
        assert!(!misaligned_ptr.is_aligned());

        let misaligned: MutSlice<u64> = MutSlice {
            // SAFETY: the pointer is non-null, points into the `data` var.
            ptr: Some(unsafe { NonNull::new_unchecked(misaligned_ptr) }),
            len: 1,
            _marker: PhantomData,
        };

        let result = misaligned.try_as_slice();
        assert!(matches!(
            result.unwrap_err(),
            SliceConversionError::MisalignedPointer
        ));
    }

    #[test]
    fn test_try_as_bytes_success() {
        let mut data = vec![65u8, 66, 67]; // "ABC"
        let slice = MutSlice::from(data.as_mut_slice());

        let result = slice.try_as_bytes();
        assert_eq!(result.unwrap(), b"ABC");
    }

    #[test]
    fn test_try_as_bytes_error_propagation() {
        let null_nonzero: MutSlice<u8> = MutSlice {
            ptr: None,
            len: 3,
            _marker: PhantomData,
        };

        let result = null_nonzero.try_as_bytes();
        assert!(matches!(
            result.unwrap_err(),
            SliceConversionError::NullPointer
        ));
    }
}
