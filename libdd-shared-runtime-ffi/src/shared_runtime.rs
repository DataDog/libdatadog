// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_shared_runtime::{SharedRuntime, SharedRuntimeError};
use std::ffi::{c_char, CString};
use std::ptr::NonNull;
use std::sync::Arc;

/// Error codes for SharedRuntime FFI operations.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum SharedRuntimeErrorCode {
    /// Invalid argument provided (e.g. null handle).
    InvalidArgument,
    /// The runtime is not available or in an invalid state.
    RuntimeUnavailable,
    /// Failed to acquire a lock on internal state.
    LockFailed,
    /// A worker operation failed.
    WorkerError,
    /// Failed to create the tokio runtime.
    RuntimeCreation,
    /// Shutdown timed out.
    ShutdownTimedOut,
    /// An unexpected panic occurred inside the FFI call.
    #[cfg(feature = "catch_panic")]
    Panic,
}

/// Error returned by SharedRuntime FFI functions.
#[repr(C)]
pub struct SharedRuntimeFFIError {
    pub code: SharedRuntimeErrorCode,
    pub msg: *mut c_char,
}

impl SharedRuntimeFFIError {
    fn new(code: SharedRuntimeErrorCode, msg: &str) -> Self {
        Self {
            code,
            msg: CString::new(msg).unwrap_or_default().into_raw(),
        }
    }
}

impl From<SharedRuntimeError> for SharedRuntimeFFIError {
    fn from(err: SharedRuntimeError) -> Self {
        let code = match &err {
            SharedRuntimeError::RuntimeUnavailable => SharedRuntimeErrorCode::RuntimeUnavailable,
            SharedRuntimeError::LockFailed(_) => SharedRuntimeErrorCode::LockFailed,
            SharedRuntimeError::WorkerError(_) => SharedRuntimeErrorCode::WorkerError,
            SharedRuntimeError::RuntimeCreation(_) => SharedRuntimeErrorCode::RuntimeCreation,
            SharedRuntimeError::ShutdownTimedOut(_) => SharedRuntimeErrorCode::ShutdownTimedOut,
        };
        SharedRuntimeFFIError::new(code, &err.to_string())
    }
}

impl Drop for SharedRuntimeFFIError {
    fn drop(&mut self) {
        if !self.msg.is_null() {
            // SAFETY: `msg` is always produced by `CString::into_raw` in `new`.
            unsafe {
                drop(CString::from_raw(self.msg));
                self.msg = std::ptr::null_mut();
            }
        }
    }
}

macro_rules! panic_error {
    () => {
        Some(Box::new(SharedRuntimeFFIError::new(
            SharedRuntimeErrorCode::Panic,
            "panic",
        )))
    };
}

/// Frees a `SharedRuntimeFFIError`. After this call the pointer is invalid.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_error_free(error: Option<Box<SharedRuntimeFFIError>>) {
    catch_panic!(drop(error), ())
}

/// Create a new `SharedRuntime`.
///
/// On success writes a raw handle into `*out_handle` and returns `None`.
/// On failure leaves `*out_handle` unchanged and returns an error.
///
/// The caller owns the handle and must eventually pass it to
/// [`ddog_shared_runtime_free`] (or another consumer that takes ownership).
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_new(
    out_handle: NonNull<*const SharedRuntime>,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        match SharedRuntime::new() {
            Ok(runtime) => {
                out_handle.as_ptr().write(Arc::into_raw(Arc::new(runtime)));
                None
            }
            Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
        },
        panic_error!()
    )
}

/// Free a handle, decrementing the `Arc` strong count.
///
/// The underlying runtime may not be dropped if other components are still using it.
/// Use [`ddog_shared_runtime_shutdown`] to cleanly stop workers.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_free(handle: *const SharedRuntime) {
    catch_panic!(
        {
            if !handle.is_null() {
                // SAFETY: handle was produced by Arc::into_raw; this call takes ownership.
                drop(Arc::from_raw(handle));
            }
        },
        ()
    )
}

/// Must be called in the parent process before `fork()`.
///
/// Pauses all workers so that no background threads are running during the
/// fork, preventing deadlocks in the child process.
///
/// Returns an error if `handle` is null.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_before_fork(
    handle: *const SharedRuntime,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            if handle.is_null() {
                return Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                )));
            }
            // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
            (*handle).before_fork();
            None
        },
        panic_error!()
    )
}

/// Must be called in the parent process after `fork()`.
///
/// Restarts all workers that were paused by [`ddog_shared_runtime_before_fork`].
///
/// Returns `None` on success, or an error if workers could not be restarted.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_parent(
    handle: *const SharedRuntime,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            if handle.is_null() {
                return Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                )));
            }
            // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
            match (*handle).after_fork_parent() {
                Ok(()) => None,
                Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
            }
        },
        panic_error!()
    )
}

/// Must be called in the child process after `fork()`.
///
/// Creates a fresh tokio runtime and restarts all workers. The original
/// runtime cannot be safely reused after a fork.
///
/// Returns `None` on success, or an error if the runtime could not be
/// reinitialized.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_child(
    handle: *const SharedRuntime,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            if handle.is_null() {
                return Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                )));
            }
            // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
            match (*handle).after_fork_child() {
                Ok(()) => None,
                Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
            }
        },
        panic_error!()
    )
}

/// Shut down the `SharedRuntime`, stopping all workers.
///
/// `timeout_ms` is the maximum time to wait for workers to stop, in
/// milliseconds.  Pass `0` for no timeout.
///
/// Returns `None` on success, or `SharedRuntimeErrorCode::ShutdownTimedOut`
/// if the timeout was reached.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_shutdown(
    handle: *const SharedRuntime,
    timeout_ms: u64,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            if handle.is_null() {
                return Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                )));
            }

            let timeout = if timeout_ms > 0 {
                Some(std::time::Duration::from_millis(timeout_ms))
            } else {
                None
            };

            // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
            match (*handle).shutdown(timeout) {
                Ok(()) => None,
                Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
            }
        },
        panic_error!()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn test_new_and_free() {
        unsafe {
            let mut handle: MaybeUninit<*const SharedRuntime> = MaybeUninit::uninit();
            let err = ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            assert!(err.is_none());
            ddog_shared_runtime_free(handle.assume_init());
        }
    }

    #[test]
    fn test_before_after_fork_null() {
        unsafe {
            let err = ddog_shared_runtime_before_fork(std::ptr::null());
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);

            let err = ddog_shared_runtime_after_fork_parent(std::ptr::null());
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);

            let err = ddog_shared_runtime_after_fork_child(std::ptr::null());
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn test_fork_lifecycle() {
        unsafe {
            let mut handle: MaybeUninit<*const SharedRuntime> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_before_fork(handle);
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            let err = ddog_shared_runtime_after_fork_parent(handle);
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            ddog_shared_runtime_free(handle);
        }
    }

    #[test]
    fn test_shutdown() {
        unsafe {
            let mut handle: MaybeUninit<*const SharedRuntime> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_shutdown(handle, 0);
            assert!(err.is_none());

            ddog_shared_runtime_free(handle);
        }
    }

    #[test]
    fn test_error_free() {
        let error = Box::new(SharedRuntimeFFIError::new(
            SharedRuntimeErrorCode::InvalidArgument,
            "test error",
        ));
        unsafe { ddog_shared_runtime_error_free(Some(error)) };
    }
}
