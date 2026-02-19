// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C-compatible wrappers for [`ShmStringTable`] APIs.
//!
//! These exist primarily for `#[no_panic::no_panic]` verification: when
//! linked into a cdylib with the `no_panic` feature enabled in release mode,
//! the linker will fail if any of these functions contain a reachable panic
//! path.
//!
//! `init` uses `try_with_capacity_in` which follows the `Fallible`
//! allocation path, so it should also be panic-free.

use crate::shm_table::ShmStringTable;
use crate::string_id::ShmStringId;
use core::ptr::NonNull;

/// Initialize a new SHM string table in the given memory region.
///
/// Returns a non-null base pointer on success, or null on error
/// (region too small, allocation failure, etc.).
///
/// # Safety
/// - `region_ptr` must point to a writable, zero-initialized memory region of at least
///   `SHM_REGION_SIZE` bytes.
/// - The region must remain valid and shared (e.g. `MAP_SHARED`) for the lifetime of all users.
///   The caller owns the region and is responsible for unmapping it (e.g. `munmap`) when done.
#[cfg_attr(all(feature = "no_panic", not(debug_assertions)), no_panic::no_panic)]
#[no_mangle]
pub unsafe extern "C" fn shm_string_table_init(region_ptr: *mut u8, region_len: usize) -> *mut u8 {
    match ShmStringTable::init_ffi(region_ptr, region_len) {
        Some(table) => table.base.as_ptr(),
        None => core::ptr::null_mut(),
    }
}

/// Intern a string into the SHM string table.
///
/// Returns the `ShmStringId` 31-bit index (>= 0) on success, or -1 on error
/// (table full, arena full, or invalid input).
///
/// # Safety
/// - `base_ptr` must point to a valid, initialized SHM region (from `ShmStringTable::init`).
/// - `s_ptr` must point to valid UTF-8 bytes of length `s_len`, or be null if `s_len` is 0.
#[cfg_attr(all(feature = "no_panic", not(debug_assertions)), no_panic::no_panic)]
#[no_mangle]
pub unsafe extern "C" fn shm_string_table_intern(
    base_ptr: NonNull<u8>,
    s_ptr: *const u8,
    s_len: usize,
) -> i64 {
    let table = ShmStringTable { base: base_ptr };

    let bytes = if s_len == 0 {
        &[]
    } else if s_ptr.is_null() {
        return -1;
    } else {
        core::slice::from_raw_parts(s_ptr, s_len)
    };

    // UTF-8 validation is the caller's responsibility (per Safety contract).
    // core::str::from_utf8 has internal panic paths the optimizer cannot
    // eliminate, so we skip it here to maintain no-panic.

    table.intern_ffi(bytes)
}

/// Look up a string by id in the SHM string table.
///
/// Writes the string pointer and length to `out_ptr` and `out_len`. On
/// out-of-bounds, writes a pointer to an empty string and length 0.
///
/// # Safety
/// - `base_ptr` must point to a valid, initialized SHM region.
/// - `out_ptr` and `out_len` must be valid, writable pointers.
#[cfg_attr(all(feature = "no_panic", not(debug_assertions)), no_panic::no_panic)]
#[no_mangle]
pub unsafe extern "C" fn shm_string_table_get(
    base_ptr: NonNull<u8>,
    id: u32,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) {
    let table = ShmStringTable { base: base_ptr };

    let shm_id = match ShmStringId::new(id) {
        Some(id) => id,
        None => {
            *out_ptr = b"".as_ptr();
            *out_len = 0;
            return;
        }
    };

    let s = table.get(shm_id);
    *out_ptr = s.as_ptr();
    *out_len = s.len();
}

/// Returns the current number of interned strings.
///
/// # Safety
/// - `base_ptr` must point to a valid, initialized SHM region.
#[cfg_attr(all(feature = "no_panic", not(debug_assertions)), no_panic::no_panic)]
#[no_mangle]
pub unsafe extern "C" fn shm_string_table_len(base_ptr: NonNull<u8>) -> u32 {
    let table = ShmStringTable { base: base_ptr };
    table.len()
}
