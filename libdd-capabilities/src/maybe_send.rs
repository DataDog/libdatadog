// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Conditional `Send` bound for cross-platform compatibility.
//!
//! On native targets, `MaybeSend` is equivalent to `Send`.
//! On wasm32, `MaybeSend` is auto-implemented for all types.
//!
//! This allows traits to require `Send` on native (for multi-threaded runtimes)
//! while remaining compatible with wasm's single-threaded execution model.
//!
//! # Why This Exists
//!
//! JavaScript interop types (like `JsFuture`, `JsValue`) are **not `Send`**
//! because wasm is single-threaded. But on native, tokio's multi-threaded
//! runtime requires `Send` futures. `MaybeSend` bridges this gap:
//!
//! ```rust,ignore
//! // Instead of:
//! fn request() -> impl Future<Output = Response> + Send;  // Won't compile on wasm!
//!
//! // Use:
//! fn request() -> impl Future<Output = Response> + MaybeSend;  // Works everywhere!
//! ```
//!
//! # Critical Rule
//!
//! **Never use `+ Send` directly in trait bounds for async functions in
//! wasm-compatible code.** Always use `+ MaybeSend` instead.

/// A trait that is `Send` on native targets, but auto-implemented on wasm.
///
/// Use this instead of `Send` in all capability trait bounds.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}

/// On wasm, `MaybeSend` is implemented for all types (no `Send` requirement).
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}

#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}
