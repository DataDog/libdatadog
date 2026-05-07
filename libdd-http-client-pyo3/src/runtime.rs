// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Python wrapper for [`libdd_shared_runtime::SharedRuntime`].
//!
//! # Why this lives in libdd-http-client-pyo3 rather than libdd-shared-runtime-ffi
//!
//! The `libdd-shared-runtime-ffi` crate exposes a C ABI only — there is no
//! pyo3 Python wrapper for `SharedRuntime` upstream of this crate. Task 8a's
//! plan calls for `HttpClient::send_blocking(&self, &SharedRuntime)`, which
//! requires a Python-visible `SharedRuntime`.
//!
//! Two paths were considered:
//!
//! 1. **Add pyo3 bindings to `libdd-shared-runtime-ffi`.** Cleaner long-term,
//!    but pulls pyo3 (a Python build-time dep) into a crate whose only
//!    consumer today is the FFI staticlib. That blast-radius change deserves
//!    its own PR.
//! 2. **Ship a thin `SharedRuntime` `#[pyclass]` in this crate.** Smaller
//!    blast radius; matches the M6 deliverable; `Arc<SharedRuntime>` is
//!    `Send + Sync` so wrapping it as a pyclass is mechanical.
//!
//! Path 2 is taken here. A follow-up may relocate the wrapper into a
//! `libdd-shared-runtime-pyo3` crate (or feature-gate it inside
//! `libdd-shared-runtime-ffi`) once a second pyo3 consumer appears.
//!
//! # Lifecycle
//!
//! Python callers must:
//!
//! 1. Construct the runtime once at process start: `runtime = SharedRuntime()`.
//! 2. Pass `runtime` to every `HttpClient.send_blocking` call.
//! 3. Around `os.fork()`, call `runtime.before_fork()` in the parent; in the
//!    child call `after_fork_child()`; in the parent post-fork call
//!    `after_fork_parent()`. (See `libdd-shared-runtime` for the full
//!    contract.)
//! 4. At process shutdown, call `runtime.shutdown(timeout_secs=None)` to stop
//!    background workers.

use crate::errors::IoError;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use std::sync::Arc;
use std::time::Duration;

create_exception!(libdd_http_client, SharedRuntimeError, PyException);

/// Python-visible wrapper around `Arc<libdd_shared_runtime::SharedRuntime>`.
#[pyclass(name = "SharedRuntime", module = "libdd_http_client", from_py_object)]
#[derive(Clone)]
pub struct SharedRuntime {
    inner: Arc<libdd_shared_runtime::SharedRuntime>,
}

#[pymethods]
impl SharedRuntime {
    /// Construct a fresh `SharedRuntime` backed by a new tokio runtime.
    #[new]
    pub fn new() -> PyResult<Self> {
        let inner = libdd_shared_runtime::SharedRuntime::new()
            .map_err(|e| SharedRuntimeError::new_err(format!("{e:?}")))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Pause workers prior to a `fork()` in the parent.
    fn before_fork(&self) {
        self.inner.before_fork();
    }

    /// Restart workers after `fork()` in the parent process.
    fn after_fork_parent(&self) -> PyResult<()> {
        self.inner
            .after_fork_parent()
            .map_err(|e| SharedRuntimeError::new_err(format!("{e:?}")))
    }

    /// Re-initialise the runtime after `fork()` in the child process.
    fn after_fork_child(&self) -> PyResult<()> {
        self.inner
            .after_fork_child()
            .map_err(|e| SharedRuntimeError::new_err(format!("{e:?}")))
    }

    /// Shut down the runtime and all workers. `timeout_secs=None` disables
    /// the timeout.
    #[pyo3(signature = (timeout_secs=None))]
    fn shutdown(&self, timeout_secs: Option<f64>) -> PyResult<()> {
        let timeout = match timeout_secs {
            None => None,
            Some(s) if s.is_finite() && s >= 0.0 => Some(Duration::from_secs_f64(s)),
            Some(s) => {
                return Err(IoError::new_err(format!(
                    "timeout_secs must be non-negative finite number, got {s}"
                )))
            }
        };
        self.inner
            .shutdown(timeout)
            .map_err(|e| SharedRuntimeError::new_err(format!("{e:?}")))
    }
}

impl SharedRuntime {
    /// Borrow the inner `SharedRuntime` for use with
    /// [`libdd_http_client::HttpClient::send_blocking`].
    pub fn as_inner(&self) -> &libdd_shared_runtime::SharedRuntime {
        &self.inner
    }
}
