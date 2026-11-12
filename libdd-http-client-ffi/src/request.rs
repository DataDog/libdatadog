// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::{HttpClientError, HttpClientErrorCode};
use crate::gen_error;
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
pub(crate) fn str_arg(slice: CharSlice<'_>) -> Result<&str, Option<Box<HttpClientError>>> {
    let bytes = slice
        .try_as_bytes()
        .map_err(|_| gen_error!(HttpClientErrorCode::InvalidArgument))?;
    core::str::from_utf8(bytes).map_err(|_| gen_error!(HttpClientErrorCode::InvalidArgument))
}

pub(crate) fn bytes_arg(slice: ByteSlice<'_>) -> Result<&[u8], Option<Box<HttpClientError>>> {
    slice
        .try_as_bytes()
        .map_err(|_| gen_error!(HttpClientErrorCode::InvalidArgument))
}

pub(crate) fn headers_arg(
    headers: Slice<'_, HttpHeader<'_>>,
) -> Result<Vec<(String, String)>, Option<Box<HttpClientError>>> {
    let headers = headers
        .try_as_slice()
        .map_err(|_| gen_error!(HttpClientErrorCode::InvalidArgument))?;
    headers
        .iter()
        .map(|header| {
            Ok((
                str_arg(header.name)?.to_owned(),
                str_arg(header.value)?.to_owned(),
            ))
        })
        .collect()
}

pub(crate) fn parts_arg(
    parts: Slice<'_, HttpMultipartPart<'_>>,
) -> Result<Vec<InnerPart>, Option<Box<HttpClientError>>> {
    let parts = parts
        .try_as_slice()
        .map_err(|_| gen_error!(HttpClientErrorCode::InvalidArgument))?;
    parts
        .iter()
        .map(|part| {
            let name = str_arg(part.name)?;
            if name.is_empty() {
                return Err(gen_error!(HttpClientErrorCode::InvalidArgument));
            }
            let data = bytes_arg(part.data)?;
            let filename = str_arg(part.filename)?;
            let content_type = str_arg(part.content_type)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null;

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    fn code<T: std::fmt::Debug>(
        result: Result<T, Option<Box<HttpClientError>>>,
    ) -> HttpClientErrorCode {
        result.expect_err("an error").expect("a boxed error").code
    }

    #[test]
    fn request_headers_replace_the_defaults_of_the_same_name_case_insensitively() {
        let defaults = pairs(&[("X-Override", "default"), ("X-Keep", "default")]);
        let request = pairs(&[("x-override", "request"), ("X-Extra", "1")]);

        assert_eq!(
            merge_headers(&defaults, request),
            pairs(&[
                ("X-Keep", "default"),
                ("x-override", "request"),
                ("X-Extra", "1")
            ])
        );
    }

    #[test]
    fn a_repeated_request_header_is_kept_as_many_times_as_given() {
        let defaults = pairs(&[("X-A", "default")]);
        let request = pairs(&[("x-a", "1"), ("X-A", "2")]);

        assert_eq!(
            merge_headers(&defaults, request),
            pairs(&[("x-a", "1"), ("X-A", "2")])
        );
    }

    #[test]
    fn a_string_that_is_not_utf8_is_an_invalid_argument() {
        let invalid = [0xff, 0xfe];

        let result = str_arg(CharSlice::from_bytes(&invalid));

        assert_eq!(code(result), HttpClientErrorCode::InvalidArgument);
    }

    #[test]
    fn a_null_slice_with_a_length_is_an_invalid_argument() {
        let slice = unsafe { CharSlice::from_raw_parts(null(), 3) };

        assert_eq!(code(str_arg(slice)), HttpClientErrorCode::InvalidArgument);
    }

    #[test]
    fn a_multipart_part_needs_a_name() {
        let parts = [HttpMultipartPart {
            name: CharSlice::from(""),
            data: ByteSlice::from(b"v".as_slice()),
            filename: CharSlice::default(),
            content_type: CharSlice::default(),
        }];

        assert_eq!(
            code(parts_arg(Slice::from(&parts[..]))),
            HttpClientErrorCode::InvalidArgument
        );
    }
}
