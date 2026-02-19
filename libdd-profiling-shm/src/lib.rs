// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared-memory and local string intern tables for profiling.
//!
//! This crate provides a shared-memory string table:
//!
//! - [`ShmStringTable`]: operates on a caller-provided shared memory region (e.g., `mmap(MAP_SHARED
//!   | MAP_ANONYMOUS)`). Supports concurrent reads from multiple processes/threads. Writes are
//!   internally serialized via an atomic spinlock.
//!
//! ID model:
//! - [`ShmStringId`] is a 31-bit id stored in a `u32` (`0..=0x7fff_ffff`).
//! - The most significant bit is reserved for future use by design.
//!
//! This crate has no global state and no platform-specific dependencies beyond
//! what the caller provides (the shared memory region).

#[cfg(feature = "ffi")]
pub mod ffi;
mod fixed_allocator;
mod shm_table;
mod string_id;

pub use shm_table::{ShmStringTable, SHM_MAX_STRINGS, SHM_REGION_SIZE};
pub use string_id::{ShmStringId, MAX_STRING_ID_31BIT};
