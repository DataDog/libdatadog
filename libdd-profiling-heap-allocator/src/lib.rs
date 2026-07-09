// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rust `GlobalAlloc` wrapper that drives `libdd-heap-sampler` around each
//! allocation. Wrap any underlying allocator with [`SampledAllocator`]; on
//! each alloc/dealloc the sampler's decision/flag/USDT machinery runs
//! around the inner call.
//!
//! `SampledAllocator` is portable across targets, so callers can use it in
//! a single `#[global_allocator]` static without cfg-gating. The sampling
//! integration (USDT probes via `libdd-heap-sampler`) is Linux-only; on
//! every other target the wrapper compiles to a transparent pass-through
//! to the inner allocator.
//!
//! # Features
//!
//! * `live-heap` (off by default) — enables live-heap tracking: allocations are flagged and frees
//!   are sampled, so a profiler can balance allocs against frees. Off = allocation profiling only.
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

mod allocator;

pub use allocator::SampledAllocator;
