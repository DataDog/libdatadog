// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_shared_runtime::{
    BlockOnTimeoutError, BlockingRuntime, ForkSafeRuntime, SharedRuntime, SharedRuntimeError,
    WorkerHandle, WorkerHandleError,
};
use std::ffi::{CString, c_char};
use std::ptr::NonNull;
use std::sync::Arc;

/// Error codes for ForkSafeRuntime FFI operations.
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
    /// The worker has already been stopped.
    WorkerStopped,
    /// The worker was dropped before completing the flush.
    FlushAborted,
    /// The flush did not complete before the timeout.
    FlushTimedOut,
    /// An unexpected panic occurred inside the FFI call.
    #[cfg(feature = "catch_panic")]
    Panic,
}

/// Error returned by ForkSafeRuntime FFI functions.
#[repr(C)]
pub struct SharedRuntimeFFIError {
    pub code: SharedRuntimeErrorCode,
    /// The error message is always defined when the error is returned by a ddog_shared_runtime
    /// ffi.
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

impl From<WorkerHandleError> for SharedRuntimeFFIError {
    fn from(err: WorkerHandleError) -> Self {
        let code = match &err {
            WorkerHandleError::AlreadyStopped => SharedRuntimeErrorCode::WorkerStopped,
            WorkerHandleError::WorkerError(_) => SharedRuntimeErrorCode::WorkerError,
            WorkerHandleError::FlushAborted => SharedRuntimeErrorCode::FlushAborted,
        };
        SharedRuntimeFFIError::new(code, &err.to_string())
    }
}

impl From<BlockOnTimeoutError> for SharedRuntimeFFIError {
    fn from(err: BlockOnTimeoutError) -> Self {
        let code = match &err {
            BlockOnTimeoutError::Io(_) => SharedRuntimeErrorCode::RuntimeUnavailable,
            BlockOnTimeoutError::TimedOut(_) => SharedRuntimeErrorCode::FlushTimedOut,
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_error_free(error: Option<Box<SharedRuntimeFFIError>>) {
    catch_panic!(drop(error), ())
}

/// Create a new `ForkSafeRuntime`.
///
/// On success writes a raw handle into `*out_handle` and returns `None`.
/// On failure leaves `*out_handle` unchanged and returns an error.
///
/// The caller owns the handle and must eventually pass it to
/// [`ddog_shared_runtime_free`] (or another consumer that takes ownership).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_new(
    out_handle: NonNull<*const ForkSafeRuntime>,
) -> Option<Box<SharedRuntimeFFIError>> {
    unsafe {
        catch_panic!(
            match ForkSafeRuntime::new() {
                Ok(runtime) => {
                    out_handle.as_ptr().write(Arc::into_raw(Arc::new(runtime)));
                    None
                }
                Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
            },
            panic_error!()
        )
    }
}

/// Free a handle, decrementing the `Arc` strong count.
///
/// The underlying runtime may not be dropped if other components are still using it.
/// Use [`ddog_shared_runtime_shutdown`] to cleanly stop workers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_free(handle: *const ForkSafeRuntime) {
    unsafe {
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
}

/// Must be called in the parent process before `fork()`.
///
/// Pauses all workers so that no background threads are running during the
/// fork, preventing deadlocks in the child process.
///
/// Returns an error if `handle` is null.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_before_fork(
    handle: Option<&ForkSafeRuntime>,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            match handle {
                Some(runtime) => {
                    // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                    runtime.before_fork();
                    None
                }
                None => Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                ))),
            }
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_parent(
    handle: Option<&ForkSafeRuntime>,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            match handle {
                Some(runtime) => {
                    // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                    match runtime.after_fork_parent() {
                        Ok(()) => None,
                        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
                    }
                }
                None => Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                ))),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_after_fork_child(
    handle: Option<&ForkSafeRuntime>,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            match handle {
                Some(runtime) => {
                    // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                    match runtime.after_fork_child() {
                        Ok(()) => None,
                        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
                    }
                }
                None => Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                ))),
            }
        },
        panic_error!()
    )
}

/// Shut down the `ForkSafeRuntime`, stopping all workers.
///
/// `timeout_ms` is the maximum time to wait for workers to stop, in
/// milliseconds.  Pass `0` for no timeout.
///
/// Returns `None` on success, or `SharedRuntimeErrorCode::ShutdownTimedOut`
/// if the timeout was reached.
/// The handle must have been initialized with `ddog_shared_runtime_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_shutdown(
    handle: Option<&ForkSafeRuntime>,
    timeout_ms: u64,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            match handle {
                Some(runtime) => {
                    let timeout = if timeout_ms > 0 {
                        Some(std::time::Duration::from_millis(timeout_ms))
                    } else {
                        None
                    };

                    // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                    match runtime.shutdown(timeout) {
                        Ok(()) => None,
                        Err(err) => Some(Box::new(SharedRuntimeFFIError::from(err))),
                    }
                }
                None => Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                ))),
            }
        },
        panic_error!()
    )
}

/// Force a flush of a worker and block until it completes.
///
/// Requests a flush on `worker` and blocks the calling thread on `runtime` until the worker has
/// flushed. `timeout_ms` bounds the wait, in milliseconds; pass `0` for no timeout.
///
/// Must not be called between [`ddog_shared_runtime_before_fork`] and
/// `ddog_shared_runtime_after_fork_*`: the flush is deferred until workers restart, so the call
/// would block until the timeout.
///
/// Returns `None` on success, or an error if an argument is null, the worker has been stopped,
/// the worker was dropped before flushing, or the timeout was reached.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_shared_runtime_worker_flush(
    runtime: Option<&ForkSafeRuntime>,
    worker: Option<&WorkerHandle>,
    timeout_ms: u64,
) -> Option<Box<SharedRuntimeFFIError>> {
    catch_panic!(
        {
            let (Some(runtime), Some(worker)) = (runtime, worker) else {
                return Some(Box::new(SharedRuntimeFFIError::new(
                    SharedRuntimeErrorCode::InvalidArgument,
                    "handle is null",
                )));
            };
            let token = match worker.flush() {
                Ok(token) => token,
                Err(err) => return Some(Box::new(SharedRuntimeFFIError::from(err))),
            };
            let result = if timeout_ms > 0 {
                runtime
                    .block_on_with_timeout(
                        token.wait(),
                        std::time::Duration::from_millis(timeout_ms),
                    )
                    .map_err(SharedRuntimeFFIError::from)
            } else {
                runtime.block_on(token.wait()).map_err(|err| {
                    SharedRuntimeFFIError::new(
                        SharedRuntimeErrorCode::RuntimeUnavailable,
                        &err.to_string(),
                    )
                })
            };
            match result {
                Ok(Ok(())) => None,
                Ok(Err(err)) => Some(Box::new(SharedRuntimeFFIError::from(err))),
                Err(err) => Some(Box::new(err)),
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
            let mut handle: MaybeUninit<*const ForkSafeRuntime> = MaybeUninit::uninit();
            let err = ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            assert!(err.is_none());
            ddog_shared_runtime_free(handle.assume_init());
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
            let mut handle: MaybeUninit<*const ForkSafeRuntime> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_before_fork(std::mem::transmute::<
                *const ForkSafeRuntime,
                Option<&ForkSafeRuntime>,
            >(handle));
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            let err = ddog_shared_runtime_after_fork_parent(std::mem::transmute::<
                *const ForkSafeRuntime,
                Option<&ForkSafeRuntime>,
            >(handle));
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));

            ddog_shared_runtime_free(handle);
        }
    }

    #[test]
    fn test_shutdown() {
        unsafe {
            let mut handle: MaybeUninit<*const ForkSafeRuntime> = MaybeUninit::uninit();
            ddog_shared_runtime_new(NonNull::new_unchecked(handle.as_mut_ptr()));
            let handle = handle.assume_init();

            let err = ddog_shared_runtime_shutdown(
                std::mem::transmute::<*const ForkSafeRuntime, Option<&ForkSafeRuntime>>(handle),
                0,
            );
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

    #[derive(Debug)]
    struct FlushWorker(std::sync::mpsc::Sender<()>);

    #[async_trait::async_trait]
    impl libdd_shared_runtime::Worker for FlushWorker {
        async fn run(&mut self) {}
        async fn trigger(&mut self) {
            std::future::pending::<()>().await
        }
        async fn flush(&mut self) {
            let _ = self.0.send(());
        }
    }

    // The FFI flush blocks until the worker flushed, with and without timeout, and reports a
    // stopped worker.
    #[test]
    fn test_worker_flush() {
        let runtime = ForkSafeRuntime::new().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = runtime.spawn_worker(FlushWorker(sender), true).unwrap();
        for timeout_ms in [0, 2000] {
            let err = unsafe {
                ddog_shared_runtime_worker_flush(Some(&runtime), Some(&worker), timeout_ms)
            };
            assert!(err.is_none(), "{:?}", err.map(|e| e.code));
            assert!(
                receiver.try_recv().is_ok(),
                "flush must complete before returning"
            );
        }
        let other = worker.clone();
        runtime.block_on(worker.stop()).unwrap().unwrap();
        let err = unsafe { ddog_shared_runtime_worker_flush(Some(&runtime), Some(&other), 0) };
        assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::WorkerStopped);
    }

    // The FFI flush rejects a null worker handle with `InvalidArgument`.
    #[test]
    fn test_worker_flush_null() {
        let runtime = ForkSafeRuntime::new().unwrap();
        let err = unsafe { ddog_shared_runtime_worker_flush(Some(&runtime), None, 0) };
        assert_eq!(err.unwrap().code, SharedRuntimeErrorCode::InvalidArgument);
    }
}
