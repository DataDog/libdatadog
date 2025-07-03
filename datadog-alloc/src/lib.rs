// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod chain;
mod linear;
mod utils;
mod virtual_alloc;

pub use chain::*;
pub use linear::*;
pub use virtual_alloc::*;

// Expose allocator_api2 for our users.
pub use allocator_api2::alloc::*;

#[cfg(feature = "alloc")]
pub use allocator_api2::boxed::*;
