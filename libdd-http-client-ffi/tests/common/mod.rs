// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Safe wrappers over the C API.
//!
//! Tests that need a server are ignored under miri: httpmock's server thread never exits, which
//! miri reports as a leak, and miri does not support Unix sockets.

#![allow(dead_code)]

use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client_ffi::*;
use libdd_shared_runtime::ForkSafeRuntime;
use std::ffi::CStr;
use std::net::{SocketAddr, TcpListener};
use std::ptr::{null, null_mut};
use std::sync::Arc;

#[derive(Debug)]
pub struct FfiError {
    pub code: HttpClientErrorCode,
    pub message: String,
}

pub fn take_error(error: Option<Box<HttpClientError>>) -> Result<(), FfiError> {
    let Some(error) = error else { return Ok(()) };
    let code = error.code;
    let message = unsafe { CStr::from_ptr(error.message) }
        .to_string_lossy()
        .into_owned();
    unsafe { ddog_http_client_error_free(Some(error)) };
    Err(FfiError { code, message })
}

#[derive(Clone, Debug)]
pub struct Cfg {
    pub base_url: String,
    pub timeout_ms: u64,
    pub unix_socket_path: String,
    pub default_headers: Vec<(String, String)>,
    pub periodic: bool,
    pub max_retries: u32,
    pub retry_initial_delay_ms: u64,
}

impl Cfg {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout_ms: 5_000,
            unix_socket_path: String::new(),
            default_headers: Vec::new(),
            periodic: false,
            max_retries: 0,
            retry_initial_delay_ms: 0,
        }
    }

    pub fn for_addr(addr: SocketAddr) -> Self {
        Self::new(format!("http://{addr}"))
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    pub fn default_header(mut self, name: &str, value: &str) -> Self {
        self.default_headers.push((name.into(), value.into()));
        self
    }

    pub fn unix_socket(mut self, path: &std::path::Path) -> Self {
        self.unix_socket_path = path.to_string_lossy().into_owned();
        self
    }

    pub fn retries(mut self, max_retries: u32, initial_delay_ms: u64) -> Self {
        self.max_retries = max_retries;
        self.retry_initial_delay_ms = initial_delay_ms;
        self
    }

    pub fn build(&self) -> Result<Client, FfiError> {
        self.build_with_runtime(null())
    }

    pub fn build_with_runtime(&self, runtime: *const ForkSafeRuntime) -> Result<Client, FfiError> {
        let headers: Vec<HttpHeader<'_>> = self
            .default_headers
            .iter()
            .map(|(name, value)| HttpHeader {
                name: CharSlice::from(name.as_str()),
                value: CharSlice::from(value.as_str()),
            })
            .collect();
        let config = HttpClientConfig {
            base_url: CharSlice::from(self.base_url.as_str()),
            timeout_ms: self.timeout_ms,
            unix_socket_path: CharSlice::from(self.unix_socket_path.as_str()),
            default_headers: Slice::from(&headers),
            periodic: self.periodic,
            max_retries: self.max_retries,
            retry_initial_delay_ms: self.retry_initial_delay_ms,
        };
        let mut out: *mut HttpClient = null_mut();
        let error = unsafe { ddog_http_client_new(&config, runtime, &mut out) };
        take_error(error)?;
        Ok(Client(out))
    }
}

/// Clients are documented as usable from several threads, hence `Send` and `Sync`.
pub struct Client(pub *mut HttpClient);

unsafe impl Send for Client {}
unsafe impl Sync for Client {}

impl Client {
    pub fn send(&self, request: &Req) -> Result<Response, FfiError> {
        self.send_with(request, None)
    }

    pub fn send_with(&self, request: &Req, cancel: Option<&Token>) -> Result<Response, FfiError> {
        let cancel_ptr: *const HttpCancellationToken = cancel.map_or(null(), |t| t.0.cast_const());
        let mut out: *mut HttpResponse = null_mut();
        let error = request.with_ffi(|request| unsafe {
            ddog_http_client_send(self.0, request, cancel_ptr, &mut out)
        });
        take_error(error)?;
        Ok(Response(out))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe {
            ddog_http_client_drop(if self.0.is_null() {
                None
            } else {
                Some(Box::from_raw(self.0))
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub data: Vec<u8>,
    pub filename: String,
    pub content_type: String,
}

impl Part {
    pub fn new(name: &str, data: &[u8]) -> Self {
        Self {
            name: name.into(),
            data: data.to_vec(),
            filename: String::new(),
            content_type: String::new(),
        }
    }

    pub fn filename(mut self, filename: &str) -> Self {
        self.filename = filename.into();
        self
    }

    pub fn content_type(mut self, content_type: &str) -> Self {
        self.content_type = content_type.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct Req {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub parts: Vec<Part>,
    pub timeout_ms: u64,
}

impl Req {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            parts: Vec::new(),
            timeout_ms: 0,
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Get, url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new(HttpMethod::Post, url)
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn part(mut self, part: Part) -> Self {
        self.parts.push(part);
        self
    }

    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    pub fn with_ffi<R>(&self, f: impl FnOnce(&HttpRequest<'_>) -> R) -> R {
        let headers: Vec<HttpHeader<'_>> = self
            .headers
            .iter()
            .map(|(name, value)| HttpHeader {
                name: CharSlice::from(name.as_str()),
                value: CharSlice::from(value.as_str()),
            })
            .collect();
        let parts: Vec<HttpMultipartPart<'_>> = self
            .parts
            .iter()
            .map(|part| HttpMultipartPart {
                name: CharSlice::from(part.name.as_str()),
                data: ByteSlice::from(part.data.as_slice()),
                filename: CharSlice::from(part.filename.as_str()),
                content_type: CharSlice::from(part.content_type.as_str()),
            })
            .collect();
        let request = HttpRequest {
            method: self.method,
            url: CharSlice::from(self.url.as_str()),
            headers: Slice::from(&headers),
            body: ByteSlice::from(self.body.as_slice()),
            multipart: Slice::from(&parts),
            timeout_ms: self.timeout_ms,
        };
        f(&request)
    }
}

pub struct Response(pub *mut HttpResponse);

impl Response {
    pub fn status(&self) -> u16 {
        unsafe { ddog_http_response_status(self.0) }
    }

    pub fn body(&self) -> Vec<u8> {
        unsafe { ddog_http_response_body(self.0) }
            .as_slice()
            .to_vec()
    }

    pub fn headers(&self) -> Vec<(String, String)> {
        unsafe { ddog_http_response_headers(self.0) }
            .as_slice()
            .iter()
            .map(|header| {
                (
                    String::from_utf8_lossy(header.name.as_bytes()).into_owned(),
                    String::from_utf8_lossy(header.value.as_bytes()).into_owned(),
                )
            })
            .collect()
    }

    /// `None` when the API returns an empty slice.
    pub fn header(&self, name: &str) -> Option<String> {
        let value = unsafe { ddog_http_response_header_get(self.0, CharSlice::from(name)) };
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        (!value.is_empty()).then_some(value)
    }
}

impl Drop for Response {
    fn drop(&mut self) {
        unsafe {
            ddog_http_response_drop(if self.0.is_null() {
                None
            } else {
                Some(Box::from_raw(self.0))
            })
        }
    }
}

pub struct Token(pub *mut HttpCancellationToken);

unsafe impl Send for Token {}
unsafe impl Sync for Token {}

impl Token {
    pub fn new() -> Self {
        Self(Box::into_raw(
            ddog_http_cancel_token_new().expect("token creation failed"),
        ))
    }

    pub fn cancel(&self) -> bool {
        unsafe { ddog_http_cancel_token_cancel(self.0) }
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        unsafe { ddog_http_cancel_token_drop(Some(Box::from_raw(self.0))) }
    }
}

/// Mimics the `Arc::into_raw` pointer that `ddog_shared_runtime_new` returns.
pub fn new_shared_runtime() -> *const ForkSafeRuntime {
    Arc::into_raw(Arc::new(
        ForkSafeRuntime::with_worker_threads(1).expect("runtime creation failed"),
    ))
}

/// # Safety
/// `runtime` must come from [`new_shared_runtime`] and not have been freed.
pub unsafe fn free_shared_runtime(runtime: *const ForkSafeRuntime) {
    drop(unsafe { Arc::from_raw(runtime) });
}

pub fn refused_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("no local addr");
    drop(listener);
    addr
}
