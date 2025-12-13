// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profile_status::{string_try_shrink_to_fit, ProfileStatus};
use libdd_common::error::FfiSafeErrorMessage;
use libdd_common_ffi::slice::SliceConversionError;
use libdd_profiling::profiles::collections::{ArcOverflow, SetError};
use libdd_profiling::profiles::FallibleStringWriter;
use std::borrow::Cow;
use std::ffi::{CStr, CString};
use std::fmt;
use std::io::ErrorKind;

/// Represents errors which can occur in the profiling FFI. Its main purpose
/// is to hold a more Rust-friendly version of [`ProfileStatus`].
#[derive(Debug)]
pub enum ProfileError {
    AllocError,
    CapacityOverflow,
    ReferenceCountOverflow,

    Other(Cow<'static, CStr>),
}

/// Represents an error that means the handle is empty, meaning it doesn't
/// point to a resource.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct EmptyHandleError;

impl From<&'static CStr> for ProfileError {
    fn from(s: &'static CStr) -> ProfileError {
        Self::Other(Cow::Borrowed(s))
    }
}

impl From<CString> for ProfileError {
    fn from(s: CString) -> ProfileError {
        Self::Other(Cow::Owned(s))
    }
}

impl From<ProfileError> for Cow<'static, CStr> {
    fn from(err: ProfileError) -> Cow<'static, CStr> {
        match err {
            ProfileError::AllocError => Cow::Borrowed(c"memory allocation failed because the memory allocator returned an error"),
            ProfileError::CapacityOverflow => Cow::Borrowed(c"memory allocation failed because the computed capacity exceeded the collection's maximum"),
            ProfileError::ReferenceCountOverflow => Cow::Borrowed(c"reference count overflow"),
            ProfileError::Other(msg) => msg,
        }
    }
}

impl From<ProfileError> for ProfileStatus {
    fn from(err: ProfileError) -> ProfileStatus {
        let cow = <Cow<'static, CStr>>::from(err);
        match cow {
            Cow::Borrowed(borrowed) => ProfileStatus::from(borrowed),
            Cow::Owned(owned) => ProfileStatus::from(owned),
        }
    }
}

impl From<ArcOverflow> for ProfileError {
    fn from(_: ArcOverflow) -> ProfileError {
        ProfileError::ReferenceCountOverflow
    }
}

impl From<allocator_api2::collections::TryReserveError> for ProfileError {
    fn from(err: allocator_api2::collections::TryReserveError) -> ProfileError {
        match err.kind() {
            allocator_api2::collections::TryReserveErrorKind::CapacityOverflow => {
                ProfileError::CapacityOverflow
            }
            allocator_api2::collections::TryReserveErrorKind::AllocError { .. } => {
                ProfileError::AllocError
            }
        }
    }
}

impl From<allocator_api2::alloc::AllocError> for ProfileError {
    fn from(_: allocator_api2::alloc::AllocError) -> ProfileError {
        ProfileError::AllocError
    }
}

impl From<std::collections::TryReserveError> for ProfileError {
    fn from(_: std::collections::TryReserveError) -> ProfileError {
        // We just assume it's out of memory since kind isn't stable.
        ProfileError::AllocError
    }
}

impl From<SetError> for ProfileError {
    fn from(err: SetError) -> ProfileError {
        ProfileError::Other(Cow::Borrowed(err.as_ffi_str()))
    }
}

impl From<EmptyHandleError> for ProfileError {
    fn from(err: EmptyHandleError) -> ProfileError {
        ProfileError::from(err.as_ffi_str())
    }
}

impl From<SliceConversionError> for ProfileError {
    fn from(err: SliceConversionError) -> ProfileError {
        ProfileError::from(err.as_ffi_str())
    }
}

/// # Safety
///
/// Uses c-str literal to ensure valid UTF-8 and null termination.
unsafe impl FfiSafeErrorMessage for EmptyHandleError {
    fn as_ffi_str(&self) -> &'static CStr {
        c"handle used with an interior null pointer"
    }
}

impl fmt::Display for EmptyHandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_rust_str().fmt(f)
    }
}

impl core::error::Error for EmptyHandleError {}

impl From<std::io::Error> for ProfileError {
    fn from(err: std::io::Error) -> ProfileError {
        match err.kind() {
            ErrorKind::StorageFull => ProfileError::CapacityOverflow,
            ErrorKind::WriteZero | ErrorKind::OutOfMemory => ProfileError::AllocError,
            e => {
                let mut writer = FallibleStringWriter::new();
                use core::fmt::Write;
                // Add null terminator that from_vec_with_nul expects.
                if write!(&mut writer, "{e}\0").is_ok() {
                    return ProfileError::Other(Cow::Borrowed(
                        c"memory allocation failed while trying to create an error message",
                    ));
                }
                let mut string = String::from(writer);
                // We do this to avoid the potential panic case of failed
                // allocation in CString::from_vec_with_nul.
                if string_try_shrink_to_fit(&mut string).is_err() {
                    return ProfileError::Other(Cow::Borrowed(c"memory allocation failed while trying to shrink a vec to create an error message"));
                }
                match CString::from_vec_with_nul(string.into_bytes()) {
                    Ok(cstring) => ProfileError::Other(Cow::Owned(cstring)),
                    Err(_) => ProfileError::Other(Cow::Borrowed(c"encountered an interior null byte while converting a std::io::Error into a ProfileError"))
                }
            }
        }
    }
}
