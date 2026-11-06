// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::HttpCancellationToken;
use crate::catch_panic;
use crate::error::{HttpClientError, not_implemented};
use crate::request::{HttpClientConfig, HttpRequest};
use crate::response::HttpResponse;
use libdd_shared_runtime::ForkSafeRuntime;

#[cfg(feature = "catch_panic")]
use crate::error::panic_error;

/// A client bound to one base URL and transport. Opaque.
///
/// It is safe to use the same client from several threads at once. It must not be dropped while a
/// send on it is in flight.
#[derive(Debug)]
pub struct HttpClient {
    // Mocked: the real fields (the `libdd_http_client::HttpClient`, the runtime, the base URL and
    // the default headers) arrive with the implementation.
    _private: (),
}

/// Creates a client and writes it to `*out`. Returns `NULL` on success, in which case the caller
/// owns `*out` and must release it with [`ddog_http_client_drop`]. On failure `*out` is left
/// untouched and an error is returned.
///
/// No connection is opened: the first request does that.
///
/// `runtime` selects how requests are driven:
///
/// * `NULL`: the client creates and owns a private runtime.
/// * otherwise: a `ForkSafeRuntime` pointer as produced by `ddog_shared_runtime_new`. The client
///   takes its own strong reference, so the caller may free its handle right after this call, and
///   several clients (and other libdatadog components) can share one runtime.
///
/// A client must not be used in a forked child process unless the runtime it uses follows the fork
/// protocol of `ForkSafeRuntime`.
///
/// Errors: `InvalidArgument` if `config` or `out` is `NULL` or a string is not UTF-8,
/// `InvalidConfig` if `base_url` is empty or invalid or `timeout_ms` is zero, `Runtime` if the
/// runtime cannot be created.
///
/// # Safety
///
/// * `config` must be `NULL` (rejected with `InvalidArgument`) or point to a valid
///   [`HttpClientConfig`] whose slices are valid for reads.
/// * `runtime` must be `NULL` or a pointer obtained from `ddog_shared_runtime_new`.
/// * `out` must be `NULL` (rejected with `InvalidArgument`) or valid for writes of one pointer.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_http_client_new(
    config: *const HttpClientConfig<'_>,
    runtime: *const ForkSafeRuntime,
    out: *mut *mut HttpClient,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        {
            let _ = (config, runtime, out);
            not_implemented("ddog_http_client_new")
        },
        panic_error()
    )
}

/// Releases a client and its pooled connections. Passing `NULL` is a no-op.
///
/// Responses obtained from the client remain valid after it is dropped.
///
/// # Safety
///
/// `client` must be `NULL` or a pointer returned by [`ddog_http_client_new`] that has not been
/// dropped, and no send may be in flight on it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_client_drop(client: Option<Box<HttpClient>>) {
    catch_panic!(drop(client), ())
}

/// Sends `request` and blocks the calling thread until it completes. On success returns `NULL`
/// and writes the response to `*out`, which the caller owns and must release with
/// [`crate::ddog_http_response_drop`]. On failure `*out` is left untouched.
///
/// Any received HTTP status, including 4xx and 5xx, is a success: read it with
/// [`crate::ddog_http_response_status`]. Redirects are not followed.
///
/// `cancel` may be `NULL`. If it is not, cancelling it from another thread makes this call return
/// `Cancelled` promptly; a token that is already cancelled makes it return `Cancelled` without
/// sending anything. The token is only read: it stays owned by the caller, who must keep it alive
/// until this call has returned and no thread can still cancel it.
///
/// Errors: `InvalidArgument` for a `NULL` pointer or a malformed request (see [`HttpRequest`]),
/// `ConnectionFailed`, `TimedOut`, `Cancelled`, `Io` and `Runtime` as described on
/// [`crate::HttpClientErrorCode`].
///
/// # Safety
///
/// * `client` must be `NULL` (rejected with `InvalidArgument`) or a live pointer returned by
///   [`ddog_http_client_new`].
/// * `request` must be `NULL` (rejected with `InvalidArgument`) or point to a valid [`HttpRequest`]
///   whose slices are valid for reads.
/// * `cancel` must be `NULL` or a live token pointer that stays valid until this call returns.
/// * `out` must be `NULL` (rejected with `InvalidArgument`) or valid for writes of one pointer.
#[unsafe(no_mangle)]
#[must_use]
pub unsafe extern "C" fn ddog_http_client_send(
    client: *const HttpClient,
    request: *const HttpRequest<'_>,
    cancel: *const HttpCancellationToken,
    out: *mut *mut HttpResponse,
) -> Option<Box<HttpClientError>> {
    catch_panic!(
        {
            let _ = (client, request, cancel, out);
            not_implemented("ddog_http_client_send")
        },
        panic_error()
    )
}
