// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rust `GlobalAlloc` wrapper that drives `libdd-heap-sampler` around each
//! allocation. Wrap any underlying allocator with [`SampledAllocator`]; on
//! each alloc/dealloc the sampler's decision/flag/USDT machinery runs
//! around the inner call.
//!
//! # Example
//!
//! ```no_run
//! use libdd_heap_allocator::SampledAllocator;
//! use std::alloc::System;
//!
//! #[global_allocator]
//! static ALLOC: SampledAllocator<System> = SampledAllocator::<System>::DEFAULT;
//! ```

#[cfg(unix)]
mod allocator;

#[cfg(unix)]
pub use allocator::SampledAllocator;
