// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ArcOverflow, SetError};
use crate::profiles::FallibleStringWriter;
use std::borrow::Cow;
use std::collections::TryReserveError;
use std::io;

/// Represents errors that occur in the profiling API.
///
/// The profiling API returns errors on allocation failures. This means the
/// error type needs to avoid allocating, or else it's possible to hit an
/// allocation error that it can't be reported, because the error also cannot
/// allocate.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error(transparent)]
    Http(#[from] http::Error),
    /// A parameter was incorrect, e.g., a null pointer was provided.
    #[error("invalid input`")]
    InvalidInput,
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Entity not found.
    #[error("not found")]
    NotFound,
    /// Failed to allocate memory needed for the operation.
    #[error("out of memory")]
    OutOfMemory,
    /// A reference count exceeded its safe capacity. This is not necessarily
    /// an integer overflow. For instance, a 64-bit refcount may choose to
    /// overflow once u32::MAX has been exceeded so that multiple simultaneous
    /// overflows are not likely to cause a refcount of 0, and can be
    /// decremented back safely.
    #[error("reference count overflow")]
    RefcountOverflow,
    /// The underlying container or storage is full. This is different from
    /// out of memory, because it's caused by some other limitation, such as
    /// the size being limited to 32-bit.
    #[error("storage full")]
    StorageFull,
    /// Some other error. Try to categorize all the errors, but since some
    /// things use [`io::Error`], there may be uncategorized errors.
    #[error("{0}")]
    Other(Cow<'static, str>),
}

impl ProfileError {
    pub fn other(error: impl Into<Cow<'static, str>>) -> Self {
        Self::Other(error.into())
    }

    pub fn from_thin_error<E: ddcommon::error::FfiSafeErrorMessage>(error: E) -> Self {
        Self::other(Cow::Borrowed(error.as_rust_str()))
    }

    /// Create a formatted error string. If memory allocation fails, a less
    /// helpful but statically known error is returned instead.
    ///
    /// # Example
    ///
    /// Use this with the `format_args!` macro:
    ///
    /// ```
    /// use datadog_profiling::profiles::ProfileError;
    /// use std::fmt;
    /// let i = 32usize;
    /// let _err = ProfileError::fmt(format_args!("out of bounds: {i}"));
    /// // do whatever you want with the error.
    /// ```
    #[cold]
    pub fn fmt(format_args: std::fmt::Arguments) -> Self {
        let mut fmt = FallibleStringWriter::new();
        let cow = if std::fmt::write(&mut fmt, format_args).is_ok() {
            Cow::Owned(fmt.into())
        } else {
            Cow::Borrowed("memory allocation failed: failed to format an error string")
        };
        Self::Other(cow)
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

impl From<ArcOverflow> for ProfileError {
    #[cold]
    fn from(_: ArcOverflow) -> Self {
        Self::RefcountOverflow
    }
}
