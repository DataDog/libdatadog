// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod alloc;

use crate::alloc::Layout;

use core::fmt;

/// The error type for `try_reserve` methods.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TryReserveError {
    kind: TryReserveErrorKind,
}

impl TryReserveError {
    /// Details about the allocation that caused the error.
    pub fn kind(&self) -> TryReserveErrorKind {
        self.kind.clone()
    }
}

/// Details of the allocation that caused a [`TryReserveError`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TryReserveErrorKind {
    /// Error due to the computed capacity exceeding the collection's maximum.
    CapacityOverflow,

    /// The memory allocator returned an error.
    AllocError {
        /// The layout of allocation request that failed.
        layout: Layout,

        #[doc(hidden)]
        non_exhaustive: (),
    },
}

impl From<TryReserveErrorKind> for TryReserveError {
    fn from(kind: TryReserveErrorKind) -> Self {
        Self { kind }
    }
}

#[cfg(feature = "alloc")]
impl From<TryReserveError> for allocator_api2::collections::TryReserveError {
    fn from(error: TryReserveError) -> Self {
        match error.kind {
            TryReserveErrorKind::CapacityOverflow => {
                allocator_api2::collections::TryReserveErrorKind::CapacityOverflow.into()
            }
            TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            } => allocator_api2::collections::TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            }
            .into(),
        }
    }
}

#[cfg(feature = "alloc")]
impl From<allocator_api2::collections::TryReserveError> for TryReserveError {
    fn from(error: allocator_api2::collections::TryReserveError) -> Self {
        match error.kind() {
            allocator_api2::collections::TryReserveErrorKind::CapacityOverflow => {
                TryReserveErrorKind::CapacityOverflow.into()
            }
            allocator_api2::collections::TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            } => TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            }
            .into(),
        }
    }
}

impl fmt::Display for TryReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("memory allocation failed")?;
        let reason = match self.kind {
            TryReserveErrorKind::CapacityOverflow => {
                " because the computed capacity exceeded the collection's maximum"
            }
            TryReserveErrorKind::AllocError { .. } => {
                " because the memory allocator returned an error"
            }
        };
        f.write_str(reason)
    }
}

impl core::error::Error for TryReserveError {}

pub mod vec;

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;

    #[test]
    fn converts_to_allocator_api2_try_reserve_error() {
        let error: TryReserveError = TryReserveErrorKind::CapacityOverflow.into();
        let converted: allocator_api2::collections::TryReserveError = error.into();

        assert_eq!(
            converted.kind(),
            allocator_api2::collections::TryReserveErrorKind::CapacityOverflow
        );

        let layout = Layout::from_size_align(16, 8).unwrap();
        let error: TryReserveError = TryReserveErrorKind::AllocError {
            layout,
            non_exhaustive: (),
        }
        .into();
        let converted: allocator_api2::collections::TryReserveError = error.into();

        assert_eq!(
            converted.kind(),
            allocator_api2::collections::TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            }
        );
    }

    #[test]
    fn converts_from_allocator_api2_try_reserve_error() {
        let error: allocator_api2::collections::TryReserveError =
            allocator_api2::collections::TryReserveErrorKind::CapacityOverflow.into();
        let converted: TryReserveError = error.into();

        assert_eq!(converted.kind(), TryReserveErrorKind::CapacityOverflow);

        let layout = Layout::from_size_align(16, 8).unwrap();
        let error: allocator_api2::collections::TryReserveError =
            allocator_api2::collections::TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            }
            .into();
        let converted: TryReserveError = error.into();

        assert_eq!(
            converted.kind(),
            TryReserveErrorKind::AllocError {
                layout,
                non_exhaustive: (),
            }
        );
    }
}
