// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! C ABI for [`libdd_http_client::HttpRequest`].
//!
//! Requests are owned by C as opaque `Box<HttpRequest>` pointers and are
//! consumed by [`crate::ddog_http_client_send_blocking`].

use crate::error::DdogHttpClientError;
use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client::{HttpMethod, HttpRequest};
use std::ptr::NonNull;
use std::time::Duration;

/// FFI mirror of [`libdd_http_client::HttpMethod`].
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DdogHttpMethod {
    /// `GET`
    Get,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
    /// `HEAD`
    Head,
    /// `PATCH`
    Patch,
    /// `OPTIONS`
    Options,
}

impl From<DdogHttpMethod> for HttpMethod {
    fn from(value: DdogHttpMethod) -> Self {
        match value {
            DdogHttpMethod::Get => HttpMethod::Get,
            DdogHttpMethod::Post => HttpMethod::Post,
            DdogHttpMethod::Put => HttpMethod::Put,
            DdogHttpMethod::Delete => HttpMethod::Delete,
            DdogHttpMethod::Head => HttpMethod::Head,
            DdogHttpMethod::Patch => HttpMethod::Patch,
            DdogHttpMethod::Options => HttpMethod::Options,
        }
    }
}

impl From<HttpMethod> for DdogHttpMethod {
    fn from(value: HttpMethod) -> Self {
        match value {
            HttpMethod::Get => DdogHttpMethod::Get,
            HttpMethod::Post => DdogHttpMethod::Post,
            HttpMethod::Put => DdogHttpMethod::Put,
            HttpMethod::Delete => DdogHttpMethod::Delete,
            HttpMethod::Head => DdogHttpMethod::Head,
            HttpMethod::Patch => DdogHttpMethod::Patch,
            HttpMethod::Options => DdogHttpMethod::Options,
        }
    }
}

/// A single HTTP header (name + value).
///
/// Both fields must contain valid UTF-8. The slices are borrowed for the
/// duration of the call that consumes them.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct DdogHttpHeader<'a> {
    /// Header name.
    pub name: CharSlice<'a>,
    /// Header value.
    pub value: CharSlice<'a>,
}

/// A slice of [`DdogHttpHeader`] values.
pub type DdogHttpHeaderSlice<'a> = Slice<'a, DdogHttpHeader<'a>>;

/// Construct a new HTTP request.
///
/// `url` must be valid UTF-8. The new request is written into
/// `*out_handle` and owned by the caller.
///
/// # Safety
/// `url` must point to valid memory for its declared length.
/// `out_handle` must be a valid, writable pointer to an
/// uninitialised `*mut ddog_HttpRequest`.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_new(
    method: DdogHttpMethod,
    url: CharSlice,
    out_handle: NonNull<Box<HttpRequest>>,
) -> Option<Box<DdogHttpClientError>> {
    let url_str = match url.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "url is not valid UTF-8: {e}"
            ))))
        }
    };
    let req = HttpRequest::new(method.into(), url_str);
    out_handle.as_ptr().write(Box::new(req));
    None
}

/// Update the HTTP method on an existing request, preserving headers,
/// body, and timeout.
///
/// # Safety
/// `request` must be `None` or a valid mutable reference to a request
/// produced by [`ddog_http_request_new`].
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_set_method(
    request: Option<&mut HttpRequest>,
    method: DdogHttpMethod,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    // HttpRequest fields are crate-private, so go through the public
    // getters: clone what we need, then construct a fresh request.
    let url = r.url().to_owned();
    let headers = r.headers().to_vec();
    let body = r.body().clone();
    let timeout = r.timeout();
    let parts = r.multipart_parts().to_vec();

    let mut new_req = HttpRequest::new(method.into(), url);
    *new_req.headers_mut() = headers;
    *new_req.body_mut() = body;
    if let Some(t) = timeout {
        new_req = new_req.with_timeout(t);
    }
    *new_req.multipart_parts_mut() = parts;
    *r = new_req;
    None
}

/// Set the request body to the given bytes (any byte sequence; not
/// required to be UTF-8). Replaces any previously set body. Empty body is
/// represented by a `body` slice of length zero.
///
/// # Safety
/// `request` must be valid; `body` must point to valid memory for its
/// declared length.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_set_body(
    request: Option<&mut HttpRequest>,
    body: ByteSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    let bytes = bytes::Bytes::copy_from_slice(body.as_bytes());
    *r.body_mut() = bytes;
    None
}

/// Append a single header to the request.
///
/// `name` and `value` must be valid UTF-8. Header names are not
/// case-folded; HTTP semantics are preserved by the underlying backend.
///
/// # Safety
/// `request` must be valid; `name` and `value` must point to valid memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_with_header(
    request: Option<&mut HttpRequest>,
    name: CharSlice,
    value: CharSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    let name_str = match name.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "header name is not valid UTF-8: {e}"
            ))))
        }
    };
    let value_str = match value.try_to_utf8() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "header value is not valid UTF-8: {e}"
            ))))
        }
    };
    r.headers_mut().push((name_str, value_str));
    None
}

/// Append multiple headers in one call.
///
/// `headers` is a slice of (name, value) pairs. Duplicate header names
/// are preserved in insertion order.
///
/// # Safety
/// `request` must be valid; `headers` must point to a valid array of
/// `DdogHttpHeader` for its declared length, and each header's `name` /
/// `value` must point to valid UTF-8 memory.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_with_headers(
    request: Option<&mut HttpRequest>,
    headers: DdogHttpHeaderSlice,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    let pairs = match headers.try_as_slice() {
        Ok(s) => s,
        Err(e) => {
            return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                "headers slice invalid: {e:?}"
            ))))
        }
    };
    for h in pairs {
        let name_str = match h.name.try_to_utf8() {
            Ok(s) => s.to_owned(),
            Err(e) => {
                return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                    "header name is not valid UTF-8: {e}"
                ))))
            }
        };
        let value_str = match h.value.try_to_utf8() {
            Ok(s) => s.to_owned(),
            Err(e) => {
                return Some(Box::new(DdogHttpClientError::invalid_argument(&format!(
                    "header value is not valid UTF-8: {e}"
                ))))
            }
        };
        r.headers_mut().push((name_str, value_str));
    }
    None
}

/// Set a per-request timeout (overriding the client-level default).
///
/// # Safety
/// `request` must be valid.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_set_timeout(
    request: Option<&mut HttpRequest>,
    timeout_ms: u64,
) -> Option<Box<DdogHttpClientError>> {
    let Some(r) = request else {
        return Some(Box::new(DdogHttpClientError::invalid_argument(
            "request is null",
        )));
    };
    let taken = std::mem::replace(r, HttpRequest::new(HttpMethod::Get, String::new()));
    *r = taken.with_timeout(Duration::from_millis(timeout_ms));
    None
}

/// Drop a request that was not consumed by `send_blocking`.
///
/// # Safety
/// `request` must be `None` or a request produced by
/// [`ddog_http_request_new`] and not yet consumed.
#[no_mangle]
pub unsafe extern "C" fn ddog_http_request_drop(request: Option<Box<HttpRequest>>) {
    drop(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn cs(s: &str) -> CharSlice<'_> {
        CharSlice::from(s)
    }

    #[test]
    fn build_request_round_trip() {
        unsafe {
            let mut req: MaybeUninit<Box<HttpRequest>> = MaybeUninit::uninit();
            let err = ddog_http_request_new(
                DdogHttpMethod::Post,
                cs("http://localhost/v0.4/traces"),
                NonNull::new_unchecked(req.as_mut_ptr()),
            );
            assert!(err.is_none());
            let mut req = req.assume_init();

            let err = ddog_http_request_with_header(
                Some(&mut req),
                cs("Content-Type"),
                cs("application/json"),
            );
            assert!(err.is_none());

            let body = b"{\"hello\":\"world\"}";
            let err = ddog_http_request_set_body(
                Some(&mut req),
                ByteSlice::from(body.as_slice()),
            );
            assert!(err.is_none());

            let err = ddog_http_request_set_timeout(Some(&mut req), 5000);
            assert!(err.is_none());

            assert_eq!(req.method(), HttpMethod::Post);
            assert_eq!(req.url(), "http://localhost/v0.4/traces");
            assert_eq!(req.headers().len(), 1);
            assert_eq!(req.body().as_ref(), body.as_slice());
            assert_eq!(req.timeout(), Some(Duration::from_millis(5000)));

            ddog_http_request_drop(Some(req));
        }
    }

    #[test]
    fn set_method_preserves_other_fields() {
        unsafe {
            let mut req: MaybeUninit<Box<HttpRequest>> = MaybeUninit::uninit();
            ddog_http_request_new(
                DdogHttpMethod::Get,
                cs("http://localhost/info"),
                NonNull::new_unchecked(req.as_mut_ptr()),
            );
            let mut req = req.assume_init();
            let _ = ddog_http_request_with_header(Some(&mut req), cs("X-A"), cs("B"));
            let _ = ddog_http_request_set_body(Some(&mut req), ByteSlice::from(b"x".as_slice()));

            let err = ddog_http_request_set_method(Some(&mut req), DdogHttpMethod::Put);
            assert!(err.is_none());

            assert_eq!(req.method(), HttpMethod::Put);
            assert_eq!(req.url(), "http://localhost/info");
            assert_eq!(req.headers().len(), 1);
            assert_eq!(req.body().as_ref(), b"x");

            ddog_http_request_drop(Some(req));
        }
    }

    #[test]
    fn with_headers_appends_in_order() {
        unsafe {
            let mut req: MaybeUninit<Box<HttpRequest>> = MaybeUninit::uninit();
            ddog_http_request_new(
                DdogHttpMethod::Get,
                cs("http://localhost"),
                NonNull::new_unchecked(req.as_mut_ptr()),
            );
            let mut req = req.assume_init();

            let pairs = [
                DdogHttpHeader {
                    name: cs("X-A"),
                    value: cs("1"),
                },
                DdogHttpHeader {
                    name: cs("X-A"),
                    value: cs("2"),
                },
            ];
            let slice =
                Slice::<DdogHttpHeader<'_>>::from_raw_parts(pairs.as_ptr(), pairs.len());
            let err = ddog_http_request_with_headers(Some(&mut req), slice);
            assert!(err.is_none());

            let h = req.headers();
            assert_eq!(h.len(), 2);
            assert_eq!(h[0].1, "1");
            assert_eq!(h[1].1, "2");

            ddog_http_request_drop(Some(req));
        }
    }
}
