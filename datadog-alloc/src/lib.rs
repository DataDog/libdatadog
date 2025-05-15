// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(feature = "std")]
extern crate std;

pub mod buffer;
mod chain;
mod linear;
mod utils;
pub mod vec;
mod virtual_alloc;

pub use chain::*;
pub use linear::*;
pub use virtual_alloc::*;

// Expose certain allocator_api2 things for our users.
pub use allocator_api2::alloc::{AllocError, Allocator, Layout, LayoutError};
use core::{error, fmt};

/// This exists because [alloc::collections::TryReserveError] hides the
/// necessary constructors and such for us to work with them ourselves.
#[repr(C)]
#[derive(Debug)]
pub enum TryReserveError {
    CapacityOverflow,
    AllocError,
}

impl fmt::Display for TryReserveError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        let reason = match self {
            TryReserveError::CapacityOverflow => {
                "memory allocation failed because the computed capacity exceeded the collection's maximum"
            }
            TryReserveError::AllocError  => {
                "memory allocation failed because the memory allocator returned an error"
            }
        };
        fmt.write_str(reason)
    }
}

impl error::Error for TryReserveError {}

#[repr(C)]
#[derive(Debug)]
pub struct NeedsCapacity {
    pub available: usize,
    pub needed: usize,
}

impl NeedsCapacity {
    #[inline]
    #[cold]
    pub const fn cold_err(available: usize, needed: usize) -> Result<(), Self> {
        Err(Self { available, needed })
    }
}

impl fmt::Display for NeedsCapacity {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            fmt,
            "operation needs more capacity; {available} available, needed {needed}",
            available = self.available,
            needed = self.needed
        )
    }
}

impl error::Error for NeedsCapacity {}
