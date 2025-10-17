// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling2::profiles::collections::{ParallelStringSet, StringId2};
use datadog_profiling2::profiles::ProfileError;
use ddcommon::error::FfiSafeErrorMessage;
use ddcommon_ffi::slice::{AsBytes, CharSlice, SliceConversionError};
use std::borrow::Cow;
use std::collections::TryReserveError;
use std::ffi::CStr;
use std::str::Utf8Error;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

impl From<Utf8ConversionError> for ProfileError {
    fn from(err: Utf8ConversionError) -> ProfileError {
        match err {
            Utf8ConversionError::OutOfMemory(_) => ProfileError::OutOfMemory,
            Utf8ConversionError::SliceConversionError(_) => ProfileError::InvalidInput,
            Utf8ConversionError::Utf8Error(_) => ProfileError::InvalidInput,
        }
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
) -> Result<StringId2, ProfileError> {
    let bytes = str.try_as_bytes().map_err(ProfileError::from_thin_error)?;
    let string = match utf8_options {
        Utf8Option::Assume => {
            // SAFETY: the caller is asserting the data is valid UTF-8.
            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(bytes) })
        }
        Utf8Option::ConvertLossy => try_from_utf8_lossy(bytes)?,
        Utf8Option::Validate => {
            Cow::Borrowed(std::str::from_utf8(bytes).map_err(|_| ProfileError::InvalidInput)?)
        }
    };
    Ok(set.try_insert(string.as_ref())?)
}
