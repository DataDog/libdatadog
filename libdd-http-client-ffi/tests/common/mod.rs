// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Safe wrappers over the C API, and a small in-process HTTP server that records requests.

#![allow(dead_code)]

use libdd_common_ffi::slice::{AsBytes, ByteSlice};
use libdd_common_ffi::{CharSlice, Slice};
use libdd_http_client_ffi::*;
use libdd_shared_runtime::ForkSafeRuntime;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Duration;

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
        // reqwest panics when building a client without an installed crypto provider.
        #[cfg(feature = "reqwest-backend")]
        let _ = rustls::crypto::ring::default_provider().install_default();

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

    pub fn header_count(&self) -> usize {
        unsafe { ddog_http_response_header_count(self.0) }
    }

    pub fn header_at(&self, index: usize) -> Option<(String, String)> {
        let mut name = CharSlice::default();
        let mut value = CharSlice::default();
        let found = unsafe { ddog_http_response_header_at(self.0, index, &mut name, &mut value) };
        found.then(|| {
            (
                String::from_utf8_lossy(name.as_bytes()).into_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
    }

    pub fn headers(&self) -> Vec<(String, String)> {
        (0..self.header_count())
            .filter_map(|i| self.header_at(i))
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

/// Header names are lower-cased.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.header_values(name).into_iter().next()
    }

    pub fn header_values(&self, name: &str) -> Vec<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

pub enum Behavior {
    /// Read the request, then write these bytes and close.
    Respond(Vec<u8>),
    /// Read the request, then close without answering.
    CloseImmediately,
    /// Read the request, then never answer (until the server is dropped).
    Hang,
}

/// `Connection: close` keeps the client from reusing a socket the server closes.
pub fn http_response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        404 => "Not Found",
        415 => "Unsupported Media Type",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    for (name, value) in headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
    out
}

pub fn respond_ok(_connection: usize) -> Behavior {
    Behavior::Respond(http_response(200, &[], b"ok"))
}

pub struct Server {
    addr: Option<SocketAddr>,
    requests: Receiver<Recorded>,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn addr(&self) -> SocketAddr {
        self.addr.expect("not a TCP server")
    }

    pub fn cfg(&self) -> Cfg {
        Cfg::for_addr(self.addr())
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr())
    }

    pub fn recv(&self) -> Recorded {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("no request reached the server")
    }

    pub fn nothing_within(&self, wait: Duration) -> bool {
        self.requests.recv_timeout(wait).is_err()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// `handler` gets the index of each accepted connection and decides what to do with it.
pub fn spawn_tcp(handler: impl Fn(usize) -> Behavior + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("no local addr");
    listener.set_nonblocking(true).expect("set_nonblocking");
    let (tx, requests) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_loop = stop.clone();
    thread::spawn(move || {
        let mut connection = 0;
        while !stop_loop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let behavior = handler(connection);
                    connection += 1;
                    spawn_connection(stream, behavior, tx.clone(), stop_loop.clone());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    Server {
        addr: Some(addr),
        requests,
        stop,
    }
}

#[cfg(unix)]
pub fn spawn_uds(
    path: &std::path::Path,
    handler: impl Fn(usize) -> Behavior + Send + 'static,
) -> Server {
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(path).expect("bind failed");
    listener.set_nonblocking(true).expect("set_nonblocking");
    let (tx, requests) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_loop = stop.clone();
    thread::spawn(move || {
        let mut connection = 0;
        while !stop_loop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let behavior = handler(connection);
                    connection += 1;
                    spawn_connection(stream, behavior, tx.clone(), stop_loop.clone());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
    });
    Server {
        addr: None,
        requests,
        stop,
    }
}

pub fn refused_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind failed");
    let addr = listener.local_addr().expect("no local addr");
    drop(listener);
    addr
}

fn spawn_connection<S: Read + Write + Send + 'static>(
    mut stream: S,
    behavior: Behavior,
    tx: Sender<Recorded>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let Some(request) = read_request(&mut stream) else {
            return;
        };
        let _ = tx.send(request);
        match behavior {
            Behavior::Respond(bytes) => {
                let _ = stream.write_all(&bytes);
                let _ = stream.flush();
            }
            Behavior::CloseImmediately => {}
            Behavior::Hang => {
                while !stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn fill<S: Read>(stream: &mut S, pending: &mut Vec<u8>) -> Option<()> {
    let mut chunk = [0u8; 8192];
    let n = stream.read(&mut chunk).ok()?;
    if n == 0 {
        return None;
    }
    pending.extend_from_slice(&chunk[..n]);
    Some(())
}

fn read_chunked<S: Read>(stream: &mut S, mut pending: Vec<u8>) -> Option<Vec<u8>> {
    let mut decoded = Vec::new();
    loop {
        let line_end = loop {
            if let Some(position) = find(&pending, b"\r\n") {
                break position;
            }
            fill(stream, &mut pending)?;
        };
        let size_line = std::str::from_utf8(&pending[..line_end]).ok()?.trim();
        let size = usize::from_str_radix(size_line, 16).ok()?;
        pending.drain(..line_end + 2);
        while pending.len() < size + 2 {
            fill(stream, &mut pending)?;
        }
        decoded.extend_from_slice(&pending[..size]);
        pending.drain(..size + 2);
        if size == 0 {
            return Some(decoded);
        }
    }
}

fn read_request<S: Read>(stream: &mut S) -> Option<Recorded> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(position) = find(&buf, b"\r\n\r\n") {
            break position;
        }
        fill(stream, &mut buf)?;
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut body = buf[head_end + 4..].to_vec();

    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();

    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let chunked = headers.iter().any(|(name, value)| {
        name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked")
    });

    if let Some(length) = content_length {
        while body.len() < length {
            fill(stream, &mut body)?;
        }
        body.truncate(length);
    } else if chunked {
        body = read_chunked(stream, body)?;
    } else {
        body.clear();
    }

    Some(Recorded {
        method,
        target,
        headers,
        body,
    })
}
