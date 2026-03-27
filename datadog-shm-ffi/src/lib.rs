// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C FFI for cross-platform named shared memory primitives.
//!
//! Provides simple create / open / read / update / drop operations on
//! named SHM segments.  SDKs link against this instead of dealing with
//! platform-specific APIs directly.
//!
//! ## Typical usage from C
//!
//! ### Writer
//! ```c
//! DdogShmWriter *writer = NULL;
//! DdogMaybeError err = ddog_shm_create(
//!     "dd-session-42",      // name
//!     data_ptr, data_len,   // payload
//!     0,                    // capacity (0 = default 64 KiB)
//!     &writer
//! );
//! // … segment is readable by other processes …
//! ddog_shm_writer_drop(writer);
//! ```
//!
//! ### Reader
//! ```c
//! DdogShmReader *reader = NULL;
//! DdogMaybeError err = ddog_shm_open("dd-session-42", &reader);
//! if (reader != NULL) {
//!     const uint8_t *ptr = NULL;
//!     size_t len = 0;
//!     ddog_shm_read_data(reader, &ptr, &len);
//!     // … use ptr[0..len] …
//!     ddog_shm_reader_drop(reader);
//! }
//! ```

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use libdd_common_ffi as ffi;
use std::ffi::CStr;
use std::os::raw::c_char;

macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return ffi::MaybeError::Some(ffi::Error::from(format!("{e:?}"))),
        }
    };
}

/// Opaque writer handle. Keeps the SHM segment alive until dropped.
pub struct DdogShmWriter {
    inner: datadog_shm::ShmWriter,
}

/// Opaque reader handle.
pub struct DdogShmReader {
    inner: datadog_shm::ShmReader,
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Create a named SHM segment and write `data` into it.
///
/// If `capacity` is 0 the default (64 KiB) is used.
///
/// # Safety
/// - `name` must be a valid null-terminated C string.
/// - `data` must point to at least `data_len` readable bytes
///   (may be NULL if `data_len` is 0).
/// - `out` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_create(
    name: *const c_char,
    data: *const u8,
    data_len: usize,
    capacity: usize,
    out: *mut *mut DdogShmWriter,
) -> ffi::MaybeError {
    if name.is_null() || out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_create: null name or out pointer".to_string(),
        ));
    }

    let name_str = try_c!(unsafe { CStr::from_ptr(name) }
        .to_str()
        .map_err(|e| anyhow::anyhow!("{e}")));

    let bytes: &[u8] = if data.is_null() || data_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_len) }
    };

    let writer = if capacity == 0 {
        try_c!(datadog_shm::ShmWriter::create(name_str, bytes))
    } else {
        try_c!(datadog_shm::ShmWriter::create_with_capacity(
            name_str, bytes, capacity,
        ))
    };

    unsafe {
        *out = Box::into_raw(Box::new(DdogShmWriter { inner: writer }));
    }
    ffi::MaybeError::None
}

/// Overwrite the segment contents.
///
/// # Safety
/// - `writer` must have been returned by `ddog_shm_create`.
/// - `data` / `data_len` must be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_update(
    writer: *mut DdogShmWriter,
    data: *const u8,
    data_len: usize,
) -> ffi::MaybeError {
    if writer.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_update: null writer".to_string(),
        ));
    }

    let bytes: &[u8] = if data.is_null() || data_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_len) }
    };

    let w = unsafe { &mut *writer };
    try_c!(w.inner.update(bytes));
    ffi::MaybeError::None
}

/// Drop a writer, unmapping and unlinking the segment.
///
/// # Safety
/// - `writer` must have been returned by `ddog_shm_create`, or be null.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_writer_drop(writer: *mut DdogShmWriter) {
    if !writer.is_null() {
        unsafe { drop(Box::from_raw(writer)) };
    }
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// Open an existing named SHM segment for reading.
///
/// Sets `*out` to a reader handle, or to NULL if the segment does not
/// exist (this is **not** an error — the returned `MaybeError` will be
/// `None`).
///
/// # Safety
/// - `name` must be a valid null-terminated C string.
/// - `out` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_open(
    name: *const c_char,
    out: *mut *mut DdogShmReader,
) -> ffi::MaybeError {
    if name.is_null() || out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_open: null name or out pointer".to_string(),
        ));
    }

    let name_str = try_c!(unsafe { CStr::from_ptr(name) }
        .to_str()
        .map_err(|e| anyhow::anyhow!("{e}")));

    match datadog_shm::ShmReader::open(name_str) {
        Ok(Some(reader)) => {
            unsafe { *out = Box::into_raw(Box::new(DdogShmReader { inner: reader })) };
        }
        Ok(None) => {
            unsafe { *out = std::ptr::null_mut() };
        }
        Err(e) => {
            unsafe { *out = std::ptr::null_mut() };
            return ffi::MaybeError::Some(ffi::Error::from(format!("{e:?}")));
        }
    }

    ffi::MaybeError::None
}

/// Get a pointer to the non-zero data prefix in the segment.
///
/// Sets `*out_ptr` and `*out_len`. The pointer is valid until the reader
/// is dropped.
///
/// # Safety
/// - `reader`, `out_ptr`, `out_len` must all be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_read_data(
    reader: *const DdogShmReader,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> ffi::MaybeError {
    if reader.is_null() || out_ptr.is_null() || out_len.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_read_data: null argument".to_string(),
        ));
    }

    let r = unsafe { &*reader };
    let data = r.inner.data_bytes();
    unsafe {
        *out_ptr = data.as_ptr();
        *out_len = data.len();
    }
    ffi::MaybeError::None
}

/// Get a pointer to the entire mapped region (including trailing zeros).
///
/// # Safety
/// - `reader`, `out_ptr`, `out_len` must all be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_read_raw(
    reader: *const DdogShmReader,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> ffi::MaybeError {
    if reader.is_null() || out_ptr.is_null() || out_len.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_read_raw: null argument".to_string(),
        ));
    }

    let r = unsafe { &*reader };
    let bytes = r.inner.as_bytes();
    unsafe {
        *out_ptr = bytes.as_ptr();
        *out_len = bytes.len();
    }
    ffi::MaybeError::None
}

/// Drop a reader.
///
/// # Safety
/// - `reader` must have been returned by `ddog_shm_open`, or be null.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_reader_drop(reader: *mut DdogShmReader) {
    if !reader.is_null() {
        unsafe { drop(Box::from_raw(reader)) };
    }
}

// ---------------------------------------------------------------------------
// PID-keyed convenience
// ---------------------------------------------------------------------------

/// Create a PID-keyed segment (`/dd-<prefix>-<pid>`).
///
/// # Safety
/// - `prefix` must be a valid null-terminated C string.
/// - `data` / `data_len` must be valid.
/// - `out` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_create_pid_keyed(
    prefix: *const c_char,
    pid: u32,
    data: *const u8,
    data_len: usize,
    out: *mut *mut DdogShmWriter,
) -> ffi::MaybeError {
    if prefix.is_null() || out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_create_pid_keyed: null argument".to_string(),
        ));
    }

    let prefix_str = try_c!(unsafe { CStr::from_ptr(prefix) }
        .to_str()
        .map_err(|e| anyhow::anyhow!("{e}")));

    let bytes: &[u8] = if data.is_null() || data_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, data_len) }
    };

    let writer = try_c!(datadog_shm::create_pid_keyed(prefix_str, pid, bytes));

    unsafe {
        *out = Box::into_raw(Box::new(DdogShmWriter { inner: writer }));
    }
    ffi::MaybeError::None
}

/// Open a PID-keyed segment.  Sets `*out` to NULL if it doesn't exist.
///
/// # Safety
/// - `prefix` must be a valid null-terminated C string.
/// - `out` must be a valid pointer.
#[no_mangle]
pub unsafe extern "C" fn ddog_shm_open_pid_keyed(
    prefix: *const c_char,
    pid: u32,
    out: *mut *mut DdogShmReader,
) -> ffi::MaybeError {
    if prefix.is_null() || out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_shm_open_pid_keyed: null argument".to_string(),
        ));
    }

    let prefix_str = try_c!(unsafe { CStr::from_ptr(prefix) }
        .to_str()
        .map_err(|e| anyhow::anyhow!("{e}")));

    match datadog_shm::open_pid_keyed(prefix_str, pid) {
        Ok(Some(reader)) => {
            unsafe { *out = Box::into_raw(Box::new(DdogShmReader { inner: reader })) };
        }
        Ok(None) => {
            unsafe { *out = std::ptr::null_mut() };
        }
        Err(e) => {
            unsafe { *out = std::ptr::null_mut() };
            return ffi::MaybeError::Some(ffi::Error::from(format!("{e:?}")));
        }
    }

    ffi::MaybeError::None
}

/// Return the current process ID.
#[no_mangle]
pub extern "C" fn ddog_shm_current_pid() -> u32 {
    datadog_shm::current_pid()
}

/// Return the parent process ID.
#[no_mangle]
pub extern "C" fn ddog_shm_parent_pid() -> u32 {
    datadog_shm::parent_pid()
}
