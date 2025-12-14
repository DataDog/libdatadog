// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::ProfileError;
use libdd_common::error::FfiSafeErrorMessage;
use libdd_common_ffi::slice::{AsBytes, CharSlice, SliceConversionError};
use libdd_profiling::profiles::collections::{ParallelStringSet, StringRef};
use std::borrow::Cow;
use std::collections::TryReserveError;
use std::ffi::CStr;
use std::str::Utf8Error;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[allow(dead_code)] // these are made through ffi
pub enum Utf8Option {
    /// The string is assumed to be valid UTF-8. If it's not, the behavior
    /// is undefined.
    Assume,
    /// The string is converted to UTF-8 using lossy conversion.
    ConvertLossy,
    /// The string is validated to be UTF-8. If it's not, an error is
    /// returned.
    Validate,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum Utf8ConversionError {
    OutOfMemory(TryReserveError),
    SliceConversionError(SliceConversionError),
    Utf8Error(Utf8Error),
}

impl From<TryReserveError> for Utf8ConversionError {
    fn from(e: TryReserveError) -> Self {
        Self::OutOfMemory(e)
    }
}

impl From<SliceConversionError> for Utf8ConversionError {
    fn from(e: SliceConversionError) -> Self {
        Self::SliceConversionError(e)
    }
}

impl From<Utf8Error> for Utf8ConversionError {
    fn from(e: Utf8Error) -> Self {
        Self::Utf8Error(e)
    }
}

// SAFETY: all cases are c-str literals, or delegate to the same trait.
unsafe impl FfiSafeErrorMessage for Utf8ConversionError {
    fn as_ffi_str(&self) -> &'static CStr {
        match self {
            Utf8ConversionError::OutOfMemory(_) => c"out of memory: utf8 conversion failed",
            Utf8ConversionError::SliceConversionError(err) => err.as_ffi_str(),
            Utf8ConversionError::Utf8Error(_) => c"invalid input: string was not utf-8",
        }
    }
}

impl Utf8Option {
    /// Converts a byte slice to a UTF-8 string according to the option.
    /// - Assume: Borrow without validation (caller guarantees UTF-8)
    /// - ConvertLossy: Lossy conversion with fallible allocation
    /// - Validate: Validate and borrow on success
    ///
    /// # Safety
    ///
    /// When [`Utf8Option::Assume`] is passed, it must be valid UTF-8.
    pub unsafe fn convert(self, bytes: &[u8]) -> Result<Cow<'_, str>, Utf8ConversionError> {
        // SAFETY: caller asserts validity under Assume
        Ok(match self {
            Utf8Option::Assume => Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(bytes) }),
            Utf8Option::ConvertLossy => try_from_utf8_lossy(bytes)?,
            Utf8Option::Validate => Cow::Borrowed(std::str::from_utf8(bytes)?),
        })
    }

    /// # Safety
    /// See the safety conditions on [`AsBytes::try_as_bytes`] and also
    /// [`Utf8Option::convert`]; both must be upheld.
    pub unsafe fn try_as_bytes_convert<'a, T: AsBytes<'a>>(
        self,
        t: T,
    ) -> Result<Cow<'a, str>, Utf8ConversionError> {
        let bytes = t.try_as_bytes()?;
        self.convert(bytes)
    }
}

/// Tries to convert a slice of bytes to a string. The input may have invalid
/// characters.
///
/// This is the same implementation as [`String::from_utf8_lossy`] except that
/// this uses fallible allocations.
pub fn try_from_utf8_lossy(v: &[u8]) -> Result<Cow<'_, str>, TryReserveError> {
    let mut iter = v.utf8_chunks();

    let first_valid = if let Some(chunk) = iter.next() {
        let valid = chunk.valid();
        if chunk.invalid().is_empty() {
            debug_assert_eq!(valid.len(), v.len());
            return Ok(Cow::Borrowed(valid));
        }
        valid
    } else {
        return Ok(Cow::Borrowed(""));
    };

    const REPLACEMENT: &str = "\u{FFFD}";
    const REPLACEMENT_LEN: usize = REPLACEMENT.len();

    let mut res = String::new();
    res.try_reserve(v.len())?;
    res.push_str(first_valid);
    res.try_reserve(REPLACEMENT_LEN)?;
    res.push_str(REPLACEMENT);

    for chunk in iter {
        let valid = chunk.valid();
        res.try_reserve(valid.len())?;
        res.push_str(valid);
        if !chunk.invalid().is_empty() {
            res.try_reserve(REPLACEMENT_LEN)?;
            res.push_str(REPLACEMENT);
        }
    }

    Ok(Cow::Owned(res))
}

pub fn insert_str(
    set: &ParallelStringSet,
    str: CharSlice<'_>,
    utf8_options: Utf8Option,
) -> Result<StringRef, ProfileError> {
    let string = unsafe { utf8_options.try_as_bytes_convert(str) }.map_err(|err| match err {
        Utf8ConversionError::OutOfMemory(err) => ProfileError::from(err),
        Utf8ConversionError::SliceConversionError(err) => ProfileError::from(err.as_ffi_str()),
        Utf8ConversionError::Utf8Error(_) => {
            ProfileError::from(c"tried to insert a non-UTF8 string into a ProfilesDictionary")
        }
    })?;
    Ok(set.try_insert(string.as_ref())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utf8_option_assume_valid() {
        let bytes = b"hello world";
        let result = unsafe { Utf8Option::Assume.convert(bytes) }.unwrap();
        assert_eq!(result, "hello world");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_utf8_option_validate_valid() {
        let bytes = b"hello world";
        let result = unsafe { Utf8Option::Validate.convert(bytes) }.unwrap();
        assert_eq!(result, "hello world");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_utf8_option_validate_invalid() {
        let bytes = b"hello \xFF world";
        let result = unsafe { Utf8Option::Validate.convert(bytes) };
        assert!(result.is_err());
        match result.unwrap_err() {
            Utf8ConversionError::Utf8Error(_) => (),
            _ => panic!("Expected Utf8Error"),
        }
    }

    #[test]
    fn test_utf8_option_convert_lossy_valid() {
        let bytes = b"hello world";
        let result = unsafe { Utf8Option::ConvertLossy.convert(bytes) }.unwrap();
        assert_eq!(result, "hello world");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_utf8_option_convert_lossy_invalid() {
        let bytes = b"hello \xFF world";
        let result = unsafe { Utf8Option::ConvertLossy.convert(bytes) }.unwrap();
        assert_eq!(result, "hello \u{FFFD} world");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_utf8_option_convert_lossy_multiple_invalid() {
        let bytes = b"\xFF\xFE valid \x80";
        let result = unsafe { Utf8Option::ConvertLossy.convert(bytes) }.unwrap();
        assert_eq!(result, "\u{FFFD}\u{FFFD} valid \u{FFFD}");
    }

    #[test]
    fn test_try_from_utf8_lossy_valid() {
        let result = try_from_utf8_lossy(b"valid utf8").unwrap();
        assert_eq!(result, "valid utf8");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_try_from_utf8_lossy_invalid_single() {
        let result = try_from_utf8_lossy(b"test\xFFstring").unwrap();
        assert_eq!(result, "test\u{FFFD}string");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_try_from_utf8_lossy_invalid_multiple() {
        let result = try_from_utf8_lossy(b"\xC3\x28 \xFF test").unwrap();
        // Invalid sequence at start, then valid, then another invalid
        assert!(result.contains("\u{FFFD}"));
        assert!(result.contains("test"));
    }

    #[test]
    fn test_try_from_utf8_lossy_empty() {
        let result = try_from_utf8_lossy(b"").unwrap();
        assert_eq!(result, "");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_try_from_utf8_lossy_all_valid_emoji() {
        let bytes = "Hello 👋 World 🌍".as_bytes();
        let result = try_from_utf8_lossy(bytes).unwrap();
        assert_eq!(result, "Hello 👋 World 🌍");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    #[allow(invalid_from_utf8)] // Checking error conversion
    fn test_utf8_conversion_error_display() {
        let err = Utf8ConversionError::Utf8Error(std::str::from_utf8(b"\xFF").unwrap_err());
        assert_eq!(
            err.as_ffi_str().to_str().unwrap(),
            "invalid input: string was not utf-8"
        );
    }

    #[test]
    fn test_utf8_conversion_error_from_try_reserve() {
        let mut v = vec![0u8];
        let reserve_err = v.try_reserve(isize::MAX as usize).unwrap_err();
        let err = Utf8ConversionError::from(reserve_err);

        match err {
            Utf8ConversionError::OutOfMemory(_) => (),
            _ => panic!("Expected OutOfMemory"),
        }

        assert_eq!(
            err.as_ffi_str().to_str().unwrap(),
            "out of memory: utf8 conversion failed"
        );
    }
}
