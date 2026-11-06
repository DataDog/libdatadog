// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use libdd_common_ffi::CharSlice;
use libdd_common_ffi::slice::ByteSlice;

/// A received response. Opaque; read it through the `ddog_http_response_*` functions. It owns its
/// data and does not depend on the client it came from.
#[derive(Debug)]
pub struct HttpResponse {
    // Mocked: the real field (the `libdd_http_client::HttpResponse`) arrives with the
    // implementation.
    _private: (),
}

/// Returns the HTTP status code of the response.
///
/// # Safety
///
/// `response` must be a live pointer written by [`crate::ddog_http_client_send`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_status(response: *const HttpResponse) -> u16 {
    catch_panic!(
        {
            let _ = response;
            0
        },
        0
    )
}

/// Returns the response body, valid until the response is dropped. The slice is empty if the
/// response has no body.
///
/// # Safety
///
/// `response` must be a live pointer written by [`crate::ddog_http_client_send`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_body(
    response: *const HttpResponse,
) -> ByteSlice<'static> {
    catch_panic!(
        {
            let _ = response;
            ByteSlice::default()
        },
        ByteSlice::default()
    )
}

/// Returns the number of headers of the response, counting repeated names separately.
///
/// # Safety
///
/// `response` must be a live pointer written by [`crate::ddog_http_client_send`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_count(response: *const HttpResponse) -> usize {
    catch_panic!(
        {
            let _ = response;
            0
        },
        0
    )
}

/// Reads the header at `index`, in the order received (`0 <= index < header_count`), writing its
/// name and value to `*name` and `*value`; both are valid until the response is dropped. Returns
/// `false`, writing nothing, if `index` is out of range.
///
/// # Safety
///
/// `response` must be a live pointer written by [`crate::ddog_http_client_send`]; `name` and
/// `value` must be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_at(
    response: *const HttpResponse,
    index: usize,
    name: *mut CharSlice<'static>,
    value: *mut CharSlice<'static>,
) -> bool {
    catch_panic!(
        {
            let _ = (response, index, name, value);
            false
        },
        false
    )
}

/// Returns the value of the first header called `name`, compared case-insensitively, valid until
/// the response is dropped. Returns an empty slice if there is no such header. A header present
/// with an empty value is indistinguishable from an absent one: use
/// [`ddog_http_response_header_at`] if that matters.
///
/// # Safety
///
/// `response` must be a live pointer written by [`crate::ddog_http_client_send`]; `name` must be
/// valid for reads.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_header_get(
    response: *const HttpResponse,
    name: CharSlice<'_>,
) -> CharSlice<'static> {
    catch_panic!(
        {
            let _ = (response, name);
            CharSlice::default()
        },
        CharSlice::default()
    )
}

/// Releases a response and every slice obtained from it. Passing `NULL` is a no-op.
///
/// # Safety
///
/// `response` must be `NULL` or a pointer written by [`crate::ddog_http_client_send`] that has not
/// been dropped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_response_drop(response: Option<Box<HttpResponse>>) {
    catch_panic!(drop(response), ())
}
