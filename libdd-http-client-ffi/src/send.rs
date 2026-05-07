// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Blocking send entry point.
//!
//! Mirrors the precedent in
//! `libdd-data-pipeline-ffi/src/trace_exporter.rs:457` for `Arc<SharedRuntime>`
//! handling: the caller passes a non-null `*const SharedRuntime` (obtained
//! from `ddog_shared_runtime_new`), we increment the strong count and
//! reconstruct an independent `Arc` via `Arc::from_raw`, then drive the
//! async `HttpClient::send` through `SharedRuntime::block_on`.

use crate::error::DdogHttpClientError;
use libdd_http_client::{HttpClient, HttpRequest, HttpResponse};
use libdd_shared_runtime::SharedRuntime;
use std::ptr::NonNull;
use std::sync::Arc;

/// Send a request synchronously, blocking until the response is received.
///
/// On success writes a `Box<HttpResponse>` into `*out_response` and returns
/// `None`. On failure returns the error and leaves `*out_response`
/// unchanged. The `request` is consumed by the call (regardless of success
/// or failure) and must not be reused or freed by the caller.
///
/// # Parameters
/// - `client`: a client produced by [`crate::ddog_http_client_builder_build`].
/// - `request`: a request produced by [`crate::ddog_http_request_new`].
/// - `shared_runtime`: a shared-runtime handle obtained via
///   `ddog_shared_runtime_new` (in `libdd-shared-runtime-ffi`). This call
///   does *not* take ownership of the handle: the caller must still
///   eventually free it via `ddog_shared_runtime_free`.
/// - `out_response`: where to write the resulting `Box<HttpResponse>`.
///
/// # Safety
/// - `client` must be a valid reference (non-null) to a client that has
///   not been dropped.
/// - `request` must be a valid `Box<HttpRequest>` produced by this crate.
/// - `shared_runtime` must be a non-null pointer produced by
///   `ddog_shared_runtime_new` whose underlying `Arc` is still alive.
/// - `out_response` must be a valid, writable pointer to an
///   uninitialised `*mut ddog_HttpResponse`.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_client_send_blocking(
    client: Option<&HttpClient>,
    request: Option<Box<HttpRequest>>,
    shared_runtime: Option<NonNull<SharedRuntime>>,
    out_response: NonNull<Box<HttpResponse>>,
) -> Option<Box<DdogHttpClientError>> {
    let Some(client) = client else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "client is null",
        )));
    };
    let Some(request) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    let Some(handle) = shared_runtime else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "shared_runtime is null",
        )));
    };

    // Mirrors libdd-data-pipeline-ffi/src/trace_exporter.rs:457: bump the
    // strong count and reconstruct an independent Arc so the caller's
    // handle remains valid and must still be freed by them.
    Arc::increment_strong_count(handle.as_ptr());
    let runtime: Arc<SharedRuntime> = Arc::from_raw(handle.as_ptr());

    let result = client.send_blocking(*request, runtime.as_ref());

    // Drop our local Arc; the caller's still owns the original.
    drop(runtime);

    match result {
        Ok(response) => {
            out_response.as_ptr().write(Box::new(response));
            None
        }
        Err(err) => Some(Box::new(DdogHttpClientError::from(err))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{ddog_http_request_new, DdogHttpMethod};
    use libdd_common_ffi::CharSlice;
    use std::mem::MaybeUninit;

    fn ensure_crypto_provider() {
        crate::client::ddog_http_client_install_default_crypto_provider();
    }

    #[test]
    fn null_client_returns_error() {
        unsafe {
            let mut req: MaybeUninit<Box<HttpRequest>> = MaybeUninit::uninit();
            ddog_http_request_new(
                DdogHttpMethod::Get,
                CharSlice::from("http://localhost"),
                NonNull::new_unchecked(req.as_mut_ptr()),
            );
            let req = req.assume_init();

            let runtime = SharedRuntime::new().unwrap();
            let arc = Arc::new(runtime);
            let raw = Arc::into_raw(arc);

            let mut resp: MaybeUninit<Box<HttpResponse>> = MaybeUninit::uninit();
            let err = ddog_http_client_send_blocking(
                None,
                Some(req),
                NonNull::new(raw as *mut SharedRuntime),
                NonNull::new_unchecked(resp.as_mut_ptr()),
            );
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );

            // Reclaim the runtime arc.
            drop(Arc::from_raw(raw));
        }
    }

    #[test]
    fn null_request_returns_error() {
        ensure_crypto_provider();
        unsafe {
            let client = HttpClient::new(
                "http://localhost".to_owned(),
                std::time::Duration::from_secs(1),
            )
            .unwrap();

            let runtime = SharedRuntime::new().unwrap();
            let arc = Arc::new(runtime);
            let raw = Arc::into_raw(arc);

            let mut resp: MaybeUninit<Box<HttpResponse>> = MaybeUninit::uninit();
            let err = ddog_http_client_send_blocking(
                Some(&client),
                None,
                NonNull::new(raw as *mut SharedRuntime),
                NonNull::new_unchecked(resp.as_mut_ptr()),
            );
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );

            drop(Arc::from_raw(raw));
        }
    }

    // End-to-end round-trip coverage lives in `examples/ffi/http_client.c`,
    // exercised via `cargo ffi-test`. Unit tests here cover the FFI
    // boundary's null/argument validation paths only.
}
