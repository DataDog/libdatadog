// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// pyo3's `create_exception!` macro generates a struct without doc-comments;
// we describe each exception in the module docstring below instead.
#![allow(missing_docs)]

//! Python exception hierarchy for [`libdd_http_client::HttpClientError`].
//!
//! All variants share a common base class `HttpClientError(Exception)` so
//! callers can `except HttpClientError` to catch any failure from this
//! module. Per-variant subclasses let callers distinguish specific failure
//! modes:
//!
//! | Rust variant                      | Python class            | Extra attributes |
//! |-----------------------------------|-------------------------|------------------|
//! | `HttpClientError::ConnectionFailed` | `ConnectionFailedError` | — |
//! | `HttpClientError::TimedOut`       | `TimedOutError`         | — |
//! | `HttpClientError::RequestFailed`  | `RequestFailedError`    | `status: int`, `body: bytes` |
//! | `HttpClientError::InvalidConfig`  | `InvalidConfigError`    | — |
//! | `HttpClientError::IoError`        | `IoError`               | — |
//!
//! `RequestFailedError` carries `status` and `body` because retry logic in
//! Task 8b will need to inspect them; today's Python callers can already
//! access them via `err.status` / `err.body` after `except RequestFailedError`.
//! Note that the underlying Rust variant stores `body` as a `String`
//! (lossy-decoded UTF-8) — we expose it as `bytes` on the Python side because
//! that's what the dd-trace-py team asked for in design discussions and it
//! preserves the option to make the Rust side switch to raw bytes later
//! without breaking the Python ABI.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(libdd_http_client, HttpClientError, PyException);
create_exception!(
    libdd_http_client,
    ConnectionFailedError,
    HttpClientError
);
create_exception!(libdd_http_client, TimedOutError, HttpClientError);
create_exception!(libdd_http_client, RequestFailedError, HttpClientError);
create_exception!(libdd_http_client, InvalidConfigError, HttpClientError);
create_exception!(libdd_http_client, IoError, HttpClientError);

/// Map a [`libdd_http_client::HttpClientError`] to the corresponding Python
/// exception class.
///
/// `RequestFailed` carries the status code and body — we surface those on the
/// Python exception by setting `status` and `body` attributes after the
/// instance is constructed.
pub fn map_http_client_error(err: libdd_http_client::HttpClientError) -> PyErr {
    use libdd_http_client::HttpClientError as E;
    match err {
        E::ConnectionFailed(msg) => ConnectionFailedError::new_err(msg),
        E::TimedOut => TimedOutError::new_err("request timed out".to_owned()),
        E::RequestFailed { status, body } => {
            // Build the exception, then attach status/body attributes.
            // pyo3 0.28's `new_err` returns a `PyErr`; we restore the value
            // to attach attributes.
            Python::attach(|py| {
                let err_value = RequestFailedError::new_err(format!(
                    "request failed with status {status}: {body}"
                ));
                let bound_err = err_value.value(py);
                if let Err(set_err) = bound_err.setattr("status", status) {
                    return set_err;
                }
                if let Err(set_err) = bound_err.setattr("body", body.into_bytes()) {
                    return set_err;
                }
                err_value
            })
        }
        E::InvalidConfig(msg) => InvalidConfigError::new_err(msg),
        E::IoError(msg) => IoError::new_err(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_http_client::HttpClientError as E;

    fn init_py() {
        // pyo3 0.28: `Python::initialize()` is the test entry point —
        // calling it more than once is a no-op. (Earlier pyo3 used
        // `prepare_freethreaded_python`; that function was renamed.)
        Python::initialize();
    }

    #[test]
    fn maps_connection_failed() {
        init_py();
        let err = map_http_client_error(E::ConnectionFailed("refused".to_owned()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<ConnectionFailedError>(py));
            assert!(err.is_instance_of::<HttpClientError>(py));
        });
    }

    #[test]
    fn maps_timed_out() {
        init_py();
        let err = map_http_client_error(E::TimedOut);
        Python::attach(|py| {
            assert!(err.is_instance_of::<TimedOutError>(py));
        });
    }

    #[test]
    fn maps_request_failed_attaches_status_and_body() {
        init_py();
        let err = map_http_client_error(E::RequestFailed {
            status: 503,
            body: "service unavailable".to_owned(),
        });
        Python::attach(|py| {
            assert!(err.is_instance_of::<RequestFailedError>(py));
            let bound = err.value(py);
            let status: u16 = bound.getattr("status").unwrap().extract().unwrap();
            assert_eq!(status, 503);
            let body: Vec<u8> = bound.getattr("body").unwrap().extract().unwrap();
            assert_eq!(body, b"service unavailable");
        });
    }

    #[test]
    fn maps_invalid_config() {
        init_py();
        let err = map_http_client_error(E::InvalidConfig("bad".to_owned()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<InvalidConfigError>(py));
        });
    }

    #[test]
    fn maps_io_error() {
        init_py();
        let err = map_http_client_error(E::IoError("broken".to_owned()));
        Python::attach(|py| {
            assert!(err.is_instance_of::<IoError>(py));
        });
    }
}
