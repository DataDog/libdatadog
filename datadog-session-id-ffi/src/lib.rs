// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C FFI for the cross-platform session ID shared memory carrier.
//!
//! SDKs that link against libdatadog can call these functions to create
//! and read session carriers without dealing with platform-specific
//! shared memory details.
//!
//! ## Typical usage from C
//!
//! ### Writer (parent process, before exec):
//! ```c
//! DdogSessionCarrier *carrier = NULL;
//! DdogMaybeError err = ddog_session_create(
//!     "550e8400-e29b-41d4-a716-446655440000",
//!     "660e8400-e29b-41d4-a716-446655440001", // or NULL
//!     &carrier
//! );
//! // ... keep `carrier` alive until children have read it ...
//! ddog_session_carrier_drop(carrier);
//! ```
//!
//! ### Reader (child process, at init):
//! ```c
//! DdogSessionResult result;
//! DdogMaybeError err = ddog_session_read_parent(&result);
//! if (result.found) {
//!     printf("session: %s\n", result.session_id);
//!     if (result.parent_session_id[0] != '\0') {
//!         printf("parent: %s\n", result.parent_session_id);
//!     }
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

/// Maximum length for session ID strings (including null terminator).
/// UUIDs are 36 chars; we allow some headroom.
const SESSION_ID_MAX_LEN: usize = 128;

/// Opaque handle returned by [`ddog_session_create`]. Must be kept alive
/// (not dropped) for as long as child processes need to read the session.
pub struct DdogSessionCarrier {
    _inner: datadog_session_id::SessionCarrier,
}

/// Result struct returned by [`ddog_session_read_parent`] and
/// [`ddog_session_read_pid`].
#[repr(C)]
pub struct DdogSessionResult {
    /// `true` if a session segment was found and read successfully.
    pub found: bool,
    /// The session ID string (null-terminated). Empty if `found` is false.
    pub session_id: [c_char; SESSION_ID_MAX_LEN],
    /// The parent session ID string (null-terminated). Empty if not set
    /// or if `found` is false.
    pub parent_session_id: [c_char; SESSION_ID_MAX_LEN],
}

impl Default for DdogSessionResult {
    fn default() -> Self {
        Self {
            found: false,
            session_id: [0; SESSION_ID_MAX_LEN],
            parent_session_id: [0; SESSION_ID_MAX_LEN],
        }
    }
}

fn copy_str_to_buf(src: &str, dst: &mut [c_char; SESSION_ID_MAX_LEN]) {
    let bytes = src.as_bytes();
    let copy_len = bytes.len().min(SESSION_ID_MAX_LEN - 1);
    for (i, &b) in bytes[..copy_len].iter().enumerate() {
        dst[i] = b as c_char;
    }
    dst[copy_len] = 0;
}

/// Create a session carrier for the current process.
///
/// # Safety
/// - `session_id` must be a valid null-terminated C string.
/// - `parent_session_id` may be null (no parent).
/// - `out` must be a valid pointer to a `*mut DdogSessionCarrier`.
///
/// The caller must eventually call [`ddog_session_carrier_drop`] on the
/// returned handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_session_create(
    session_id: *const c_char,
    parent_session_id: *const c_char,
    out: *mut *mut DdogSessionCarrier,
) -> ffi::MaybeError {
    if session_id.is_null() || out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_session_create: null session_id or out pointer".to_string(),
        ));
    }

    let sid = unsafe { CStr::from_ptr(session_id) };
    let sid_str = try_c!(sid.to_str().map_err(|e| anyhow::anyhow!("{e}")));

    let parent_str = if parent_session_id.is_null() {
        None
    } else {
        let psid = unsafe { CStr::from_ptr(parent_session_id) };
        Some(try_c!(psid.to_str().map_err(|e| anyhow::anyhow!("{e}"))))
    };

    let carrier = try_c!(datadog_session_id::create_session_carrier(
        sid_str,
        parent_str,
    ));

    unsafe {
        *out = Box::into_raw(Box::new(DdogSessionCarrier { _inner: carrier }));
    }

    ffi::MaybeError::None
}

/// Drop (free) a session carrier previously created with
/// [`ddog_session_create`]. After this call the shared memory segment
/// is unmapped and children can no longer read it.
///
/// # Safety
/// - `carrier` must have been returned by `ddog_session_create`, or be null.
#[no_mangle]
pub unsafe extern "C" fn ddog_session_carrier_drop(carrier: *mut DdogSessionCarrier) {
    if !carrier.is_null() {
        unsafe {
            drop(Box::from_raw(carrier));
        }
    }
}

/// Read session data from the **parent process**.
///
/// # Safety
/// - `out` must be a valid pointer to a `DdogSessionResult`.
#[no_mangle]
pub unsafe extern "C" fn ddog_session_read_parent(
    out: *mut DdogSessionResult,
) -> ffi::MaybeError {
    if out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_session_read_parent: null out pointer".to_string(),
        ));
    }

    let mut result = DdogSessionResult::default();

    match datadog_session_id::read_parent_session() {
        Ok(Some(payload)) => {
            result.found = true;
            copy_str_to_buf(&payload.session_id, &mut result.session_id);
            if let Some(ref psid) = payload.parent_session_id {
                copy_str_to_buf(psid, &mut result.parent_session_id);
            }
        }
        Ok(None) => {
            result.found = false;
        }
        Err(e) => {
            unsafe { *out = result; }
            return ffi::MaybeError::Some(ffi::Error::from(format!("{e:?}")));
        }
    }

    unsafe { *out = result; }
    ffi::MaybeError::None
}

/// Read session data from a **specific process** by PID.
///
/// # Safety
/// - `out` must be a valid pointer to a `DdogSessionResult`.
#[no_mangle]
pub unsafe extern "C" fn ddog_session_read_pid(
    pid: u32,
    out: *mut DdogSessionResult,
) -> ffi::MaybeError {
    if out.is_null() {
        return ffi::MaybeError::Some(ffi::Error::from(
            "ddog_session_read_pid: null out pointer".to_string(),
        ));
    }

    let mut result = DdogSessionResult::default();

    match datadog_session_id::read_session_for_pid(pid) {
        Ok(Some(payload)) => {
            result.found = true;
            copy_str_to_buf(&payload.session_id, &mut result.session_id);
            if let Some(ref psid) = payload.parent_session_id {
                copy_str_to_buf(psid, &mut result.parent_session_id);
            }
        }
        Ok(None) => {
            result.found = false;
        }
        Err(e) => {
            unsafe { *out = result; }
            return ffi::MaybeError::Some(ffi::Error::from(format!("{e:?}")));
        }
    }

    unsafe { *out = result; }
    ffi::MaybeError::None
}
