// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rust `GlobalAlloc` wrapper that drives `libdd-profiling-heap-sampler` around each
//! allocation. Wrap any underlying allocator with [`SampledAllocator`]; on
//! each alloc/dealloc the sampler's decision/flag/USDT machinery runs
//! around the inner call.
//!
//! `SampledAllocator` is portable across targets, so callers can use it in
//! a single `#[global_allocator]` static without cfg-gating. The sampling
//! integration (USDT probes via `libdd-profiling-heap-sampler`) is Linux-only; on
//! every other target the wrapper compiles to a transparent pass-through
//! to the inner allocator.
//!
//! # Features
//!
//! * `live-heap` (off by default) - enables live-heap tracking: sampled allocations are flagged at
//!   alloc time, and that flag is detected again on free, so a profiler can pair each free back to
//!   its sample and balance allocs against frees. Off = allocation profiling only.
//!
//! # Example
//!
//! ```no_run
//! use libdd_profiling_heap_allocator::{set_default_sampling_distance, SampledAllocator};
//! use std::alloc::System;
//!
//! #[global_allocator]
//! static ALLOC: SampledAllocator<System> = SampledAllocator::<System>::DEFAULT;
//!
//! fn main() {
//!     // Optional: tune the mean sample distance. Smaller values capture
//!     // more samples at higher runtime cost; larger values reduce overhead
//!     // but may miss short-lived allocation patterns.
//!     set_default_sampling_distance(128 * 1024);
//!     // ... application runs ...
//! }
//! ```

mod allocator;

pub use allocator::SampledAllocator;

/// Set the default mean sample distance (bytes between samples) for the
/// heap sampler.
///
/// The sampler draws from an exponential distribution around this mean,
/// so individual gaps vary but average to the configured value. Pass `0`
/// to revert to the compiled-in default (512 KiB).
///
/// Call this early in process startup, before significant allocation
/// activity begins.
#[cfg(target_os = "linux")]
pub fn set_default_sampling_distance(distance_bytes: u64) {
    libdd_profiling_heap_sampler::set_default_sampling_distance(distance_bytes);
}

/// See the Linux variant above.
#[cfg(not(target_os = "linux"))]
pub fn set_default_sampling_distance(_distance_bytes: u64) {}
