// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_data_pipeline::shared_runtime::{SharedRuntime, SharedRuntimeError};
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

/// Frees a `SharedRuntimeFFIError`. After this call the pointer is invalid.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_error_free(error: Option<Box<SharedRuntimeFFIError>>) {
    drop(error);
}

/// Create a new `SharedRuntime` wrapped in an `Arc`.
///
/// On success writes the new handle into `*out_handle` and returns `None`.
/// On failure leaves `*out_handle` unchanged and returns an error.
///
/// The caller owns the returned handle and must eventually pass it to
/// [`ddog_shared_runtime_free`] (or another consumer that takes ownership).
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_new(
    out_handle: NonNull<Box<Arc<SharedRuntime>>>,
) -> Option<Box<SharedRuntimeFFIError>> {
    match SharedRuntime::new() {
        Ok(runtime) => {
            out_handle.as_ptr().write(Box::new(Arc::new(runtime)));
            None
        }
        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
    }
}

/// Clone the `Arc<SharedRuntime>`, incrementing the reference count.
///
/// The new handle is independent from the original: either can be freed
/// without affecting the other.  The underlying runtime is only destroyed
/// when every handle has been freed.
///
/// On success writes the cloned handle into `*out_handle` and returns `None`.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_clone(
    handle: Option<&Arc<SharedRuntime>>,
    out_handle: NonNull<Box<Arc<SharedRuntime>>>,
) -> Option<Box<SharedRuntimeFFIError>> {
    match handle {
        Some(arc) => {
            out_handle.as_ptr().write(Box::new(arc.clone()));
            None
        }
        None => Some(Box::new(SharedRuntimeFFIError::new(
            SharedRuntimeErrorCode::InvalidArgument,
            "handle is null",
        ))),
    }
}

/// Free a `SharedRuntime` handle, decrementing the `Arc` reference count.
///
/// The underlying runtime is only shut down once the last handle is freed.
/// Use [`ddog_shared_runtime_shutdown`] to explicitly stop the runtime and
/// all its workers before the last handle is freed.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_free(handle: Box<Arc<SharedRuntime>>) {
    drop(handle);
}

/// Must be called in the parent process before `fork()`.
///
/// Pauses all workers so that no background threads are running during the
/// fork, preventing deadlocks in the child process.
///
/// Returns an error if `handle` is null.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_before_fork(
    handle: Option<&Arc<SharedRuntime>>,
) -> Option<Box<SharedRuntimeFFIError>> {
    let runtime = match handle {
        Some(r) => r,
        None => {
            return Some(Box::new(SharedRuntimeFFIError::new(
                SharedRuntimeErrorCode::InvalidArgument,
                "handle is null",
            )))
        }
    };

    runtime.before_fork();
    None
}

/// Must be called in the parent process after `fork()`.
///
/// Restarts all workers that were paused by [`ddog_shared_runtime_before_fork`].
///
/// Returns `None` on success, or an error if workers could not be restarted.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_parent(
    handle: Option<&Arc<SharedRuntime>>,
) -> Option<Box<SharedRuntimeFFIError>> {
    let runtime = match handle {
        Some(r) => r,
        None => {
            return Some(Box::new(SharedRuntimeFFIError::new(
                SharedRuntimeErrorCode::InvalidArgument,
                "handle is null",
            )))
        }
    };

    match runtime.after_fork_parent() {
        Ok(()) => None,
        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
    }
}

/// Must be called in the child process after `fork()`.
///
/// Creates a fresh tokio runtime and restarts all workers. The original
/// runtime cannot be safely reused after a fork.
///
/// Returns `None` on success, or an error if the runtime could not be
/// reinitialized.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_child(
    handle: Option<&Arc<SharedRuntime>>,
) -> Option<Box<SharedRuntimeFFIError>> {
    let runtime = match handle {
        Some(r) => r,
        None => {
            return Some(Box::new(SharedRuntimeFFIError::new(
                SharedRuntimeErrorCode::InvalidArgument,
                "handle is null",
            )))
        }
    };

    match runtime.after_fork_child() {
        Ok(()) => None,
        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
    }
}

/// Shut down the `SharedRuntime`, stopping all workers.
///
/// `timeout_ms` is the maximum time to wait for workers to stop, in
/// milliseconds.  Pass `0` for no timeout.
///
/// Returns `None` on success, or `SharedRuntimeErrorCode::ShutdownTimedOut`
/// if the timeout was reached.
#[no_mangle]
pub unsafe extern "C" fn ddog_shared_runtime_shutdown(
    handle: Option<&Arc<SharedRuntime>>,
    timeout_ms: u64,
) -> Option<Box<SharedRuntimeFFIError>> {
    let runtime = match handle {
        Some(r) => r,
        None => {
            return Some(Box::new(SharedRuntimeFFIError::new(
                SharedRuntimeErrorCode::InvalidArgument,
                "handle is null",
            )))
        }
    };

    let timeout = if timeout_ms > 0 {
        Some(std::time::Duration::from_millis(timeout_ms))
    } else {
        None
    };

    match runtime.shutdown(timeout) {
        Ok(()) => None,
        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    #[test]
    fn test_new_and_free() {
        unsafe {
            let mut handle: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            let err = ddog_shared_runtime_new(NonNull::new_unchecked(&mut handle).cast());
            assert!(err.is_none());
            ddog_shared_runtime_free(handle.assume_init());
        }
    }

    #[test]
    fn test_clone() {
        unsafe {
            let mut handle: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(&mut handle).cast());
            let handle = handle.assume_init();

            let mut cloned: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            let err = ddog_shared_runtime_clone(
                Some(handle.as_ref()),
                NonNull::new_unchecked(&mut cloned).cast(),
            );
            assert!(err.is_none());

            // Both handles should point to the same underlying runtime (strong count == 2).
            assert_eq!(Arc::strong_count(handle.as_ref()), 2);

            ddog_shared_runtime_free(cloned.assume_init());
            assert_eq!(Arc::strong_count(handle.as_ref()), 1);

            ddog_shared_runtime_free(handle);
        }
    }

    #[test]
    fn test_clone_null_handle() {
        unsafe {
            let mut cloned: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            let err = ddog_shared_runtime_clone(None, NonNull::new_unchecked(&mut cloned).cast());
            assert!(err.is_some());
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn test_before_after_fork_null() {
        unsafe {
            let err = ddog_shared_runtime_before_fork(None);
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);

            let err = ddog_shared_runtime_after_fork_parent(None);
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);

            let err = ddog_shared_runtime_after_fork_child(None);
            assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn test_fork_lifecycle() {
        unsafe {
            let mut handle: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(&mut handle).cast());
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_before_fork(Some(handle.as_ref()));
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            let err = ddog_shared_runtime_after_fork_parent(Some(handle.as_ref()));
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            ddog_shared_runtime_free(handle);
        }
    }

    #[test]
    fn test_shutdown() {
        unsafe {
            let mut handle: MaybeUninit<Box<Arc<SharedRuntime>>> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(&mut handle).cast());
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_shutdown(Some(handle.as_ref()), 0);
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
