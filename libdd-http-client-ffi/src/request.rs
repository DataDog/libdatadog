// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::HttpClientError;
use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client::MultipartPart as InnerPart;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpHeader<'a> {
    pub name: CharSlice<'a>,
    pub value: CharSlice<'a>,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
    Head,
    Patch,
    Options,
}

/// Read by [`crate::ddog_http_client_new`] and not retained. `base_url` and `timeout_ms` are
/// required; for every other field, zero or empty selects the default.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpClientConfig<'a> {
    /// Relative request URLs are appended to it, e.g. `http://localhost:8126`. The host is ignored
    /// when `unix_socket_path` is set.
    pub base_url: CharSlice<'a>,
    /// Default timeout of a request.
    pub timeout_ms: u64,
    /// Connect through this Unix socket instead of TCP (Unix only).
    pub unix_socket_path: CharSlice<'a>,
    /// Added to every request. A request header of the same name (case-insensitive) replaces it.
    pub default_headers: Slice<'a, HttpHeader<'a>>,
    /// For clients used for periodic flushes: pooled connections expire quickly.
    pub periodic: bool,
    /// Retries of a failed send (never of an HTTP status), with backoff. `0` disables them.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub retry_initial_delay_ms: u64,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpMultipartPart<'a> {
    /// Required.
    pub name: CharSlice<'a>,
    pub data: ByteSlice<'a>,
    /// Empty means none.
    pub filename: CharSlice<'a>,
    /// Empty means none.
    pub content_type: CharSlice<'a>,
}

/// Read by [`crate::ddog_http_client_send`] and not retained.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpRequest<'a> {
    pub method: HttpMethod,
    /// An absolute `http(s)` URL, or a path starting with `/` appended to the client's `base_url`.
    pub url: CharSlice<'a>,
    pub headers: Slice<'a, HttpHeader<'a>>,
    /// Raw request body. Mutually exclusive with `multipart`.
    pub body: ByteSlice<'a>,
    /// The client sets `Content-Type`. Mutually exclusive with `body`.
    pub multipart: Slice<'a, HttpMultipartPart<'a>>,
    /// `0` uses the client's `timeout_ms`.
    pub timeout_ms: u64,
}

/// Reads a string argument without panicking on a malformed slice.
pub(crate) fn str_arg<'a>(slice: CharSlice<'a>, name: &str) -> Result<&'a str, HttpClientError> {
    let bytes = slice.try_as_bytes().map_err(|e| {
        HttpClientError::invalid_argument(format!("{name} is not a valid slice: {e:?}"))
    })?;
    core::str::from_utf8(bytes)
        .map_err(|_| HttpClientError::invalid_argument(format!("{name} is not valid UTF-8")))
}

pub(crate) fn bytes_arg<'a>(slice: ByteSlice<'a>, name: &str) -> Result<&'a [u8], HttpClientError> {
    slice.try_as_bytes().map_err(|e| {
        HttpClientError::invalid_argument(format!("{name} is not a valid slice: {e:?}"))
    })
}

/// Header names must be HTTP tokens and values free of control characters.
pub(crate) fn headers_arg(
    headers: Slice<'_, HttpHeader<'_>>,
    name: &str,
) -> Result<Vec<(String, String)>, HttpClientError> {
    let headers = headers.try_as_slice().map_err(|e| {
        HttpClientError::invalid_argument(format!("{name} is not a valid slice: {e:?}"))
    })?;
    headers
        .iter()
        .map(|header| {
            let header_name = str_arg(header.name, "header name")?;
            let header_value = str_arg(header.value, "header value")?;
            if !is_valid_header_name(header_name) {
                return Err(HttpClientError::invalid_argument(format!(
                    "invalid header name {header_name:?}"
                )));
            }
            if !is_valid_header_value(header_value) {
                return Err(HttpClientError::invalid_argument(format!(
                    "invalid value for header {header_name:?}"
                )));
            }
            Ok((header_name.to_owned(), header_value.to_owned()))
        })
        .collect()
}

pub(crate) fn parts_arg(
    parts: Slice<'_, HttpMultipartPart<'_>>,
) -> Result<Vec<InnerPart>, HttpClientError> {
    let parts = parts.try_as_slice().map_err(|e| {
        HttpClientError::invalid_argument(format!("multipart is not a valid slice: {e:?}"))
    })?;
    parts
        .iter()
        .map(|part| {
            let name = str_arg(part.name, "multipart part name")?;
            if name.is_empty() {
                return Err(HttpClientError::invalid_argument(
                    "a multipart part needs a name",
                ));
            }
            let data = bytes_arg(part.data, "multipart part data")?;
            let filename = str_arg(part.filename, "multipart part filename")?;
            let content_type = str_arg(part.content_type, "multipart part content_type")?;
            let mut inner = InnerPart::new(name, data.to_vec());
            if !filename.is_empty() {
                inner = inner.with_filename(filename);
            }
            if !content_type.is_empty() {
                inner = inner.with_content_type(content_type);
            }
            Ok(inner)
        })
        .collect()
}

/// Request headers replace the defaults of the same name (case-insensitive).
pub(crate) fn merge_headers(
    defaults: &[(String, String)],
    request: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = defaults
        .iter()
        .filter(|(name, _)| {
            !request
                .iter()
                .any(|(request_name, _)| request_name.eq_ignore_ascii_case(name))
        })
        .cloned()
        .collect();
    merged.extend(request);
    merged
}

/// RFC 9110 `token`: the characters allowed in a header name.
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// No control characters other than tab.
fn is_valid_header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b == b'\t' || (b >= 0x20 && b != 0x7f))
}
