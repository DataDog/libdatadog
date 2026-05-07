// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_http_client::HttpResponse`].
//!
//! Responses are produced by [`crate::ddog_http_client_send_blocking`] and
//! owned by C as opaque `Box<HttpResponse>` pointers.

use crate::error::DdogHttpClientError;
use crate::request::DdogHttpHeader;
use libdd_common_ffi::CharSlice;
use libdd_http_client::HttpResponse;

/// Read the HTTP status code (e.g. 200, 404, 503).
///
/// Returns 0 if `response` is null.
///
/// # Safety
/// `response` must be `None` or a valid reference produced by
/// [`crate::ddog_http_client_send_blocking`] that has not yet been
/// dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_response_status(response: Option<&HttpResponse>) -> u16 {
    match response {
        Some(r) => r.status_code(),
        None => 0,
    }
}

/// Borrow the response body as a byte slice.
///
/// The returned pointer is valid until the response is dropped via
/// [`ddog_http_response_drop`]. If `response` is null the returned
/// pointer is null and `*out_len` is set to 0.
///
/// # Safety
/// `response` must be valid; `out_len` must be a valid mutable pointer
/// or null.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_response_body(
    response: Option<&HttpResponse>,
    out_len: Option<&mut usize>,
) -> *const u8 {
    match response {
        Some(r) => {
            let body = r.body();
            if let Some(len) = out_len {
                *len = body.len();
            }
            body.as_ptr()
        }
        None => {
            if let Some(len) = out_len {
                *len = 0;
            }
            std::ptr::null()
        }
    }
}

/// Borrow the response headers.
///
/// `out_headers` is allocated by this call and written into via
/// `*out_ptr` / `*out_len`. The header memory (the array of
/// [`DdogHttpHeader`] entries) is owned by the caller and must be
/// released via [`ddog_http_response_headers_free`]. The `name` and
/// `value` slices inside each header point into the response and remain
/// valid for the response's lifetime.
///
/// # Safety
/// `response` must be valid; `out_ptr` and `out_len` must be valid
/// writable pointers.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_response_headers<'a>(
    response: Option<&'a HttpResponse>,
    out_ptr: Option<&mut *mut DdogHttpHeader<'a>>,
    out_len: Option<&mut usize>,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = response else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "response is null",
        )));
    };
    // SAFETY: HttpResponse keeps the header strings owned for its lifetime;
    // we hand back borrowed slices that share that lifetime.
    let entries: Vec<DdogHttpHeader<'_>> = r
        .headers()
        .iter()
        .map(|(name, value)| DdogHttpHeader {
            name: CharSlice::from(name.as_str()),
            value: CharSlice::from(value.as_str()),
        })
        .collect();

    let len = entries.len();
    let mut boxed = entries.into_boxed_slice();
    let ptr = boxed.as_mut_ptr();
    // We hand off ownership of the boxed slice; it's reclaimed by
    // `ddog_http_response_headers_free` below.
    std::mem::forget(boxed);

    if let Some(out_ptr) = out_ptr {
        *out_ptr = ptr;
    } else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "out_ptr is null",
        )));
    }
    if let Some(out_len) = out_len {
        *out_len = len;
    } else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "out_len is null",
        )));
    }

    None
}

/// Free a header array previously produced by
/// [`ddog_http_response_headers`].
///
/// # Safety
/// `ptr` must have come from `ddog_http_response_headers` paired with
/// the original `len`. The associated response must still be alive (the
/// `name` / `value` slices borrow from it) — but freeing only frees the
/// outer array, so freeing after dropping the response is also safe.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_response_headers_free(
    ptr: *mut DdogHttpHeader<'_>,
    len: usize,
) {
    if ptr.is_null() || len == 0 {
        return;
    }
    let slice = std::slice::from_raw_parts_mut(ptr, len);
    drop(Box::from_raw(slice as *mut [DdogHttpHeader<'_>]));
}

/// Drop an HTTP response.
///
/// # Safety
/// `response` must be `None` or a response produced by
/// [`crate::ddog_http_client_send_blocking`] and not yet dropped.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_response_drop(response: Option<Box<HttpResponse>>) {
    drop(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_null_returns_zero() {
        unsafe {
            assert_eq!(ddog_http_response_status(None), 0);
        }
    }

    #[test]
    fn body_null_returns_null() {
        unsafe {
            let mut len = 99usize;
            let p = ddog_http_response_body(None, Some(&mut len));
            assert!(p.is_null());
            assert_eq!(len, 0);
        }
    }

    #[test]
    fn headers_null_returns_invalid_arg() {
        unsafe {
            let mut ptr: *mut DdogHttpHeader<'_> = std::ptr::null_mut();
            let mut len = 0usize;
            let err = ddog_http_response_headers(None, Some(&mut ptr), Some(&mut len));
            assert!(err.is_some());
            assert_eq!(
                err.unwrap().code,
                crate::DdogHttpClientErrorCode::InvalidArgument
            );
        }
    }

    #[test]
    fn drop_handles_none() {
        unsafe { ddog_http_response_drop(None) };
    }

    #[test]
    fn headers_free_handles_null() {
        unsafe { ddog_http_response_headers_free(std::ptr::null_mut(), 0) };
    }
}
