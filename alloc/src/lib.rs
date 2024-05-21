// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![no_std]

mod chain;
mod linear;
mod r#virtual;

pub use chain::*;
pub use linear::*;
pub use r#virtual::*;

// Expose allocator_api2 for our users.
pub use allocator_api2::alloc::*;
