// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use crate::request::str_arg;
use libdd_common_ffi::CharSlice;
use libdd_common_ffi::slice::ByteSlice;
use libdd_http_client::HttpResponse as InnerResponse;

/// Owns its data and does not depend on the client it came from.
#[derive(Debug)]
pub struct HttpResponse {
    inner: InnerResponse,
}

impl HttpResponse {
    pub(crate) fn new(inner: InnerResponse) -> Self {
        Self { inner }
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

/// Returns the number of headers, counting repeated names separately.
///
/// # Safety
///
/// `response` must be a live response pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_count(response: *const HttpResponse) -> usize {
    catch_panic!(
        unsafe { response.as_ref() }.map_or(0, |response| response.inner.headers().len()),
        0
    )
}

/// Writes the name and value of the header at `index` to `*name` and `*value`, valid until the
/// response is dropped. Returns `false`, writing nothing, if `index` is out of range.
///
/// A repeated name is returned as several entries. Headers of different names may not come back in
/// the order received.
///
/// # Safety
///
/// `response` must be a live response pointer; `name` and `value` must be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_at(
    response: *const HttpResponse,
    index: usize,
    name: *mut CharSlice<'static>,
    value: *mut CharSlice<'static>,
) -> bool {
    catch_panic!(
        {
            let header = unsafe { response.as_ref() }
                .and_then(|response| response.inner.headers().get(index));
            match header {
                Some((header_name, header_value)) if !name.is_null() && !value.is_null() => {
                    unsafe {
                        name.write(str_slice(header_name));
                        value.write(str_slice(header_value));
                    }
                    true
                }
                _ => false,
            }
        },
        false
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
