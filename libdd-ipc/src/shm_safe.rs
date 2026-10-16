// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Marker trait for types that can live in memory shared with other processes.
//!
//! Shared-memory payloads are not owned by a single Rust program:
//!
//! - A fresh mapping is zero-filled, and a recycled slot keeps whatever bytes its previous tenant
//!   wrote, so the first value observed can be *any* bit pattern .
//! - Other processes read and write the same bytes at any moment, without any synchronization: a
//!   value observed through a shared-memory reference can change between two reads, and
//!   neighbouring fields can hold bytes from different writes.
//!
//! [`ShmSafe`] encodes that contract at the type level, so that the generic shared-memory wrappers
//! in this crate can require it instead of trusting every caller.

use std::sync::atomic::AtomicU64;

/// A type whose values can be stored in, and concurrently accessed from, memory shared with
/// other processes.
///
/// # Safety
///
/// To implement [ShmSafe] for `T`, both of the following must hold:
///
/// 1. **Every bit pattern must be a valid value**. `T` in particular must not contain references, raw
///    pointers, `bool`s, `char`s, `enum`s, `NonZero*` or any other type that carries a validity
///    invariant.
/// 2. **Every field is interior-mutable.** Peers write the underlying bytes without any
///    synchronization, so each field must be an atomic, an `UnsafeCell`, or a type that is itself
///    `ShmSafe`. The wrapper's `UnsafeCell<Inner>` covers the top level, but not the fields within.
///
/// In practice, [ShmSafe] should only be implemented for zero-sized types, atomics or structs of
/// atomics.
pub unsafe trait ShmSafe: Sized {}

unsafe impl ShmSafe for () {}
unsafe impl ShmSafe for AtomicU64 {}
