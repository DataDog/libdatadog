// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetError;
use std::collections::TryReserveError;
use std::{fmt, io};

/// Represents errors that occur in the profiling API.
///
/// The profiling API returns errors on allocation failures. This means the
/// error type needs to avoid allocating, or else it's possible to hit an
/// allocation error that it can't be reported, because the error also cannot
/// allocate.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProfileError {
    /// A parameter was incorrect, e.g., a null pointer was provided.
    InvalidInput,
    /// Entity not found.
    NotFound,
    /// Failed to allocate memory needed for the operation.
    OutOfMemory,
    /// A reference count exceeded its safe capacity. This is not necessarily
    /// an integer overflow. For instance, a 64-bit refcount may choose to
    /// overflow once u32::MAX has been exceeded so that multiple simultaneous
    /// overflows are not likely to cause a refcount of 0, and can be
    /// decremented back safely.
    RefcountOverflow,
    /// The underlying container or storage is full. This is different from
    /// out of memory, because it's caused by some other limitation, such as
    /// the size being limited to 32-bit.
    StorageFull,
    /// Some other error. Try to categorize all the errors, but since some
    /// things use [`io::Error`], there may be uncategorized errors.
    Other,
}

impl ProfileError {
    pub const fn as_cstr(&self) -> &'static core::ffi::CStr {
        match self {
            ProfileError::InvalidInput => c"invalid input",
            ProfileError::NotFound => c"not found",
            ProfileError::OutOfMemory => c"out of memory",
            ProfileError::RefcountOverflow => c"reference count overflow",
            ProfileError::StorageFull => c"storage full",
            ProfileError::Other => c"unknown error",
        }
    }

    pub const fn as_str(&self) -> &'static str {
        // unwrap_unchecked is not a const fn yet
        match self.as_cstr().to_str() {
            Ok(str) => str,
            Err(_) => unsafe { core::hint::unreachable_unchecked() },
        }
    }
}

impl From<http::Error> for ProfileError {
    fn from(_: http::Error) -> Self {
        // todo: can we determine which things might be invalid inputs?
        Self::Other
    }
}

impl From<SetError> for ProfileError {
    fn from(err: SetError) -> Self {
        match err {
            SetError::InvalidArgument => ProfileError::InvalidInput,
            SetError::OutOfMemory => ProfileError::OutOfMemory,
            SetError::ReferenceCountOverflow => ProfileError::RefcountOverflow,
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl std::error::Error for ProfileError {}

impl From<io::Error> for ProfileError {
    #[cold]
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::InvalidInput => ProfileError::InvalidInput,
            io::ErrorKind::NotFound => ProfileError::NotFound,
            io::ErrorKind::OutOfMemory => ProfileError::OutOfMemory,
            io::ErrorKind::StorageFull | io::ErrorKind::WriteZero => ProfileError::StorageFull,
            _ => ProfileError::Other,
        }
    }
}

impl From<datadog_alloc::LayoutError> for ProfileError {
    fn from(_: datadog_alloc::LayoutError) -> Self {
        ProfileError::InvalidInput
    }
}

impl From<TryReserveError> for ProfileError {
    #[cold]
    fn from(_: TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

impl From<hashbrown::TryReserveError> for ProfileError {
    #[cold]
    fn from(err: hashbrown::TryReserveError) -> Self {
        match err {
            hashbrown::TryReserveError::CapacityOverflow => ProfileError::StorageFull,
            hashbrown::TryReserveError::AllocError { .. } => ProfileError::OutOfMemory,
        }
    }
}

impl From<datadog_alloc::AllocError> for ProfileError {
    #[cold]
    fn from(_: datadog_alloc::AllocError) -> Self {
        Self::OutOfMemory
    }
}

impl<T> From<arrayvec::CapacityError<T>> for ProfileError {
    fn from(_: arrayvec::CapacityError<T>) -> Self {
        Self::StorageFull
    }
}

/// A result for operations that return a ProfileError on failure, and nothing
/// on success.
#[repr(C)]
#[derive(Debug)]
pub enum ProfileVoidResult {
    Ok,
    Err(ProfileError),
}

impl From<ProfileError> for ProfileVoidResult {
    #[cold]
    fn from(error: ProfileError) -> Self {
        ProfileVoidResult::Err(error)
    }
}

impl From<Result<(), ProfileError>> for ProfileVoidResult {
    #[cold]
    fn from(result: Result<(), ProfileError>) -> Self {
        match result {
            Ok(_) => ProfileVoidResult::Ok,
            Err(err) => ProfileVoidResult::Err(err),
        }
    }
}

impl From<ProfileVoidResult> for Result<(), ProfileError> {
    #[cold]
    fn from(result: ProfileVoidResult) -> Self {
        match result {
            ProfileVoidResult::Ok => Ok(()),
            ProfileVoidResult::Err(err) => Err(err),
        }
    }
}
