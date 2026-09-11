// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Thread-level context sharing
//!
//! This crate implements the publisher side of the Thread Context OTEP (PR #4947).
//!
//! Since `rustc` doesn't currently support the TLSDESC dialect, we define the thread-local
//! storage symbol and its accessor using inline assembly (`global_asm!` / `asm!`).
//!
//! ## Ownership modes
//!
//! The crate provides exactly one of two context ownership flavours, selected by two mutually
//! exclusive features: `owned-context` (the default) gives `OwnedThreadContext`, a record owned by
//! a single thread that can be updated in place, while `shared-context` gives
//! `SharedThreadContext`, an `Arc`-backed immutable record that can be cloned and attached on
//! several threads. They can't coexist because the TLS slot is untyped. If both features are
//! enabled, `owned-context` wins and the build script emits a warning.
#![cfg_attr(
    feature = "owned-context",
    doc = "The active mode is `owned-context`. See [`linux::OwnedThreadContext`]."
)]
#![cfg_attr(
    not(feature = "owned-context"),
    doc = "The active mode is `shared-context`. See [`linux::SharedThreadContext`], which also \
           explains why the two modes can't be mixed."
)]
//!
//! ## Usage
//!
//! There are two main patterns for publishing and updating thread contexts.
//!
//! ### In-place update
//!
//! The simplest pattern, when applicable, is to attach one record and then mutate it in place.
//! This avoids allocation in the hot path.
//!
//! ```rust
//! # #[cfg(all(feature = "owned-context", target_os = "linux",
//! #           any(target_arch = "x86_64", target_arch = "aarch64")))]
//! # fn main() {
//! use libdd_otel_thread_ctx::linux::OwnedThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id = [1u8; 8];
//! let local_root_span_id = [2u8; 8];
//!
//! // First call allocates a record and attaches it.
//! OwnedThreadContext::new(trace_id, span_id, 1, local_root_span_id, &[(0, "first")]).attach();
//! // Second call update the attached record in place.
//! OwnedThreadContext::update(trace_id, span_id, 1, local_root_span_id, &[(0, "second")]);
//! // Detach and drop.
//! let _ = OwnedThreadContext::detach();
//! # }
//! # #[cfg(not(all(feature = "owned-context", target_os = "linux",
//! #               any(target_arch = "x86_64", target_arch = "aarch64"))))]
//! # fn main() {}
//! ```
//!
//! ### Swapping
//!
//! Swapping can be used when it's beneficial to pre-allocate or keep around a bunch of contexts
//! to be saved and restored repeatedly. Could be the case with async-runtimes where several tasks
//! might run on the same thread, or even move from one thread to another, for example.
//!
//! ```rust
//! # #[cfg(all(feature = "owned-context", target_os = "linux",
//! #           any(target_arch = "x86_64", target_arch = "aarch64")))]
//! # fn main() {
//! use libdd_otel_thread_ctx::linux::OwnedThreadContext;
//!
//! let trace_id = [0u8; 16];
//! let span_id = [1u8; 8];
//! let local_root_span_id = [2u8; 8];
//! let attrs: &[(u8, &str)] = &[(0, "GET"), (1, "/api/v1")];
//!
//! // Publish a new context and save the previously attached one (if any).
//! let ctx = OwnedThreadContext::new(trace_id, span_id, 1, local_root_span_id, attrs);
//! let previous = ctx.attach();
//!
//! // ... do work inside the span ...
//!
//! // Restore the previous context: detach the current one and re-attach the saved one.
//! if let Some(prev) = previous {
//!     // here we drop `ctx`, but we could store for later usage
//!     let _ = prev.attach();
//! }
//! # }
//! # #[cfg(not(all(feature = "owned-context", target_os = "linux",
//! #               any(target_arch = "x86_64", target_arch = "aarch64"))))]
//! # fn main() {}
//! ```
//!
//! ## Synchronization
//!
//! Readers are constrained to the same thread as the writer and operate like async-signal
//! handlers: the writer thread is always stopped while a reader runs. There is thus no
//! cross-thread synchronization concerns. The only hazard is compiler reordering, which is
//! handled by making `valid` atomic and using compiler-only fences (equivalent to C's
//! `atomic_signal_fence`) to keep field writes boxed between the `valid = 0` and `valid = 1`
//! stores during in-place updates.

// The `linux` module below resolves the TLS slot with TLSDESC inline assembly that is only written
// for x86_64 and aarch64. Reject any other architecture on Linux at compile time. On non-Linux
// targets the `linux` module is not compiled, so there's no such constraint.
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "x86_64", target_arch = "aarch64"))
))]
compile_error!(
    "Unsupported architecture for otel-thread-ctx on Linux. Only x86_64 and aarch64 are currently \
     supported."
);

#[cfg(all(target_os = "linux", feature = "sanity-check"))]
pub mod sanity_check;

#[cfg(feature = "test-utils")]
pub mod test_utils;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod linux;
