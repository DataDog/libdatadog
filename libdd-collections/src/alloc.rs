// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub use allocator_api2::alloc::{AllocError, Allocator};
pub use core::alloc::{Layout, LayoutError};

#[cfg(feature = "alloc")]
pub use allocator_api2::alloc::Global;
#[cfg(feature = "std")]
pub use allocator_api2::alloc::System;
#[cfg(feature = "alloc")]
pub use allocator_api2::boxed::Box;
