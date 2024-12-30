// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt;
use std::{
    ffi::c_char,
    marker::PhantomData,
    mem::{self, ManuallyDrop},
    ptr::{self, NonNull},
};

/// Ffi safe type representing a borrowed null-terminated C array
/// Equivalent to a std::ffi::CStr
#[repr(C)]
pub struct CStr<'a> {
    /// Null terminated char array
    ptr: ptr::NonNull<c_char>,
    /// Length of the array, not counting the null-terminator
    length: usize,
    _lifetime_marker: std::marker::PhantomData<&'a c_char>,
}

impl<'a> CStr<'a> {
    pub fn from_std(s: &'a std::ffi::CStr) -> Self {
        Self {
            ptr: unsafe { ptr::NonNull::new_unchecked(s.as_ptr().cast_mut()) },
            length: s.to_bytes().len() - 1,
            _lifetime_marker: std::marker::PhantomData,
        }
    }

    pub fn into_std(&self) -> &'a std::ffi::CStr {
        unsafe {
            std::ffi::CStr::from_bytes_with_nul_unchecked(std::slice::from_raw_parts(
                self.ptr.as_ptr().cast_const().cast(),
                self.length,
            ))
        }
    }
}

/// Ffi safe type representing an owned null-terminated C array
/// Equivalent to a std::ffi::CString
#[repr(C)]
pub struct CString {
    /// Null terminated char array
    ptr: ptr::NonNull<c_char>,
    /// Length of the array, not counting the null-terminator
    length: usize,
}

impl fmt::Debug for CString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_cstr().into_std().fmt(f)
    }
}

impl CString {
    pub fn new<T: Into<Vec<u8>>>(t: T) -> Result<Self, std::ffi::NulError> {
        Ok(Self::from_std(std::ffi::CString::new(t)?))
    }

    pub fn as_cstr(&self) -> CStr<'_> {
        CStr {
            ptr: self.ptr,
            length: self.length,
            _lifetime_marker: PhantomData,
        }
    }

    pub fn from_std(s: std::ffi::CString) -> Self {
        let s = ManuallyDrop::new(s);
        Self {
            ptr: unsafe { ptr::NonNull::new_unchecked(s.as_ptr().cast_mut()) },
            length: s.to_bytes().len(),
        }
    }

    pub fn into_std(self) -> std::ffi::CString {
        let s = ManuallyDrop::new(self);
        unsafe {
            std::ffi::CString::from_vec_with_nul_unchecked(Vec::from_raw_parts(
                s.ptr.as_ptr().cast(),
                s.length,
                s.length,
            ))
        }
    }
}

impl Drop for CString {
    fn drop(&mut self) {
        let ptr = mem::replace(&mut self.ptr, NonNull::dangling());
        drop(unsafe {
            std::ffi::CString::from_vec_with_nul_unchecked(Vec::from_raw_parts(
                ptr.as_ptr().cast(),
                self.length,
                self.length,
            ))
        });
    }
}
