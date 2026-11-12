// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use crate::request::{HttpHeader, str_arg};
use libdd_common_ffi::slice::ByteSlice;
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client::HttpResponse as InnerResponse;
use std::sync::OnceLock;

/// Owns its data and does not depend on the client it came from.
#[derive(Debug)]
pub struct HttpResponse {
    inner: InnerResponse,
    // Built on the first call to `ddog_http_response_headers`.
    headers: OnceLock<Vec<HttpHeader<'static>>>,
}

impl HttpResponse {
    pub(crate) fn new(inner: InnerResponse) -> Self {
        Self {
            inner,
            headers: OnceLock::new(),
        }
    }
}

/// The `'static` lifetime is nominal: the slice is valid until the response is dropped.
fn str_slice(s: &str) -> CharSlice<'static> {
    unsafe { CharSlice::from_raw_parts(s.as_ptr().cast(), s.len()) }
}

/// # Safety
///
/// `response` must be a live response pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_status(response: *const HttpResponse) -> u16 {
    catch_panic!(
        unsafe { response.as_ref() }.map_or(0, |response| response.inner.status_code()),
        0
    )
}

/// Returns the body, valid until the response is dropped.
///
/// # Safety
///
/// `response` must be a live response pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_body(
    response: *const HttpResponse,
) -> ByteSlice<'static> {
    catch_panic!(
        unsafe { response.as_ref() }.map_or(ByteSlice::default(), |response| {
            let body = response.inner.body();
            unsafe { ByteSlice::from_raw_parts(body.as_ptr(), body.len()) }
        }),
        ByteSlice::default()
    )
}

/// Returns all the headers, valid until the response is dropped. A repeated name is returned as
/// several entries. Headers of different names may not come back in the order received.
///
/// # Safety
///
/// `response` must be a live response pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_headers(
    response: *const HttpResponse,
) -> Slice<'static, HttpHeader<'static>> {
    catch_panic!(
        unsafe { response.as_ref() }.map_or(Slice::default(), |response| {
            let headers = response.headers.get_or_init(|| {
                response
                    .inner
                    .headers()
                    .iter()
                    .map(|(name, value)| HttpHeader {
                        name: str_slice(name),
                        value: str_slice(value),
                    })
                    .collect()
            });
            unsafe { Slice::from_raw_parts(headers.as_ptr(), headers.len()) }
        }),
        Slice::default()
    )
}

/// Returns the value of the first header called `name` (case-insensitive), valid until the response
/// is dropped, or an empty slice if there is none. An empty value is indistinguishable from an
/// absent header.
///
/// # Safety
///
/// `response` must be a live response pointer; `name` must be valid for reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_get(
    response: *const HttpResponse,
    name: CharSlice<'_>,
) -> CharSlice<'static> {
    catch_panic!(
        {
            let response = unsafe { response.as_ref() };
            match (response, str_arg(name)) {
                (Some(response), Ok(name)) => response
                    .inner
                    .headers()
                    .iter()
                    .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
                    .map_or(CharSlice::default(), |(_, value)| str_slice(value)),
                _ => CharSlice::default(),
            }
        },
        CharSlice::default()
    )
}

/// Releases a response and the slices obtained from it. Passing `NULL` is a no-op.
///
/// # Safety
///
/// `response` must be `NULL` or a live response pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_drop(response: Option<Box<HttpResponse>>) {
    catch_panic!(drop(response), ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_a_null_response_is_a_no_op() {
        unsafe { ddog_http_response_drop(None) };
    }
}
