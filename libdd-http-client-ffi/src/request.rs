// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common_ffi::slice::ByteSlice;
use libdd_common_ffi::{CharSlice, Slice};

/// One HTTP header. Both slices are UTF-8 and need no NUL terminator.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpHeader<'a> {
    pub name: CharSlice<'a>,
    pub value: CharSlice<'a>,
}

/// HTTP method of a request.
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

/// Configuration of a client, read by [`crate::ddog_http_client_new`] and not retained: every slice
/// only needs to stay valid for the duration of that call.
///
/// A zero-initialised struct is *not* a valid configuration: `base_url` and `timeout_ms` are
/// required. Every other field is optional and a zero or empty value selects the default.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpClientConfig<'a> {
    /// Origin requests are sent to, e.g. `http://localhost:8126` or `https://example.com/prefix`.
    /// Required. Relative request URLs (starting with `/`) are appended to it after removing its
    /// trailing slash. When `unix_socket_path` is set only the path part is used and the host is
    /// ignored for routing.
    pub base_url: CharSlice<'a>,
    /// Default timeout for a whole request, in milliseconds. Required, must be greater than zero.
    pub timeout_ms: u64,
    /// Route every connection through this Unix domain socket. Empty means plain TCP. Not
    /// supported on Windows, where a non-empty value is an `InvalidConfig` error.
    pub unix_socket_path: CharSlice<'a>,
    /// Headers added to every request. A header of the same name (case-insensitive) given on a
    /// request replaces the default instead of being added next to it.
    pub default_headers: Slice<'a, HttpHeader<'a>>,
    /// Set when the client is used for periodic one-shot flushes (traces, telemetry, ...). Pooled
    /// connections then expire after a few seconds, so a connection the receiver is about to close
    /// is not reused.
    pub periodic: bool,
    /// Retry a failed send (never a received HTTP status) up to this many times with exponential
    /// backoff and jitter. `0` disables retries.
    pub max_retries: u32,
    /// Delay before the first retry, in milliseconds. `0` selects the default (100 ms).
    pub retry_initial_delay_ms: u64,
}

/// One part of a `multipart/form-data` body.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpMultipartPart<'a> {
    /// Field name. Required.
    pub name: CharSlice<'a>,
    /// Raw content of the part.
    pub data: ByteSlice<'a>,
    /// File name reported for the part. Empty means none.
    pub filename: CharSlice<'a>,
    /// Content type of the part. Empty means none.
    pub content_type: CharSlice<'a>,
}

/// A request, read by [`crate::ddog_http_client_send`]: every slice only needs to stay valid for
/// the duration of that call.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct HttpRequest<'a> {
    pub method: HttpMethod,
    /// Either an absolute `http://` or `https://` URL (the client's `base_url` is then ignored), or
    /// a path starting with `/` (optionally with a query string) appended to the client's
    /// `base_url`. Anything else is an `InvalidArgument` error.
    pub url: CharSlice<'a>,
    /// Request headers, added to the client's default headers.
    pub headers: Slice<'a, HttpHeader<'a>>,
    /// Raw request body. Mutually exclusive with `multipart`.
    pub body: ByteSlice<'a>,
    /// Parts of a `multipart/form-data` body; the `Content-Type` header, with its boundary, is set
    /// by the client. Mutually exclusive with `body`.
    pub multipart: Slice<'a, HttpMultipartPart<'a>>,
    /// Timeout for this request, in milliseconds. `0` uses the client's `timeout_ms`.
    pub timeout_ms: u64,
}
