// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `create_exception!` does not produce documented items, so we cannot turn on
// `#![deny(missing_docs)]` at the crate root without sprinkling
// `#[allow(missing_docs)]` everywhere. Use a softer warn-level instead and
// keep the expressive lints we care about.
#![warn(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]

//! Python (pyo3) bindings for `libdd-http-client` and `libdd-agent-client`.
//!
//! Task 8a shipped the minimum `HttpClient` surface; Task 8b — this revision —
//! completes the surface so that dd-trace-py call sites can be migrated
//! end-to-end. The new surface consists of:
//!
//! - [`MultipartPart`] for `multipart/form-data` uploads.
//! - [`RetryConfig`] for opt-in automatic retry with exponential backoff.
//! - [`init_fips_crypto`] (only when compiled with the `fips` feature) to
//!   install the aws-lc-rs FIPS provider in lieu of ring at module init.
//! - The full [`agent_client`] surface (`AgentClient`, `AgentClientBuilder`,
//!   `LanguageMetadata`, `AgentTransport`, `TraceFormat`, `TraceSendOptions`,
//!   `AgentResponse`, `TelemetryRequest`, `AgentInfo`).
//!
//! # Surface
//!
//! Python classes exposed (in module `libdd_http_client`):
//!
//! - `HttpClient` — wraps [`libdd_http_client::HttpClient`].
//! - `HttpClientBuilder` — wraps [`libdd_http_client::HttpClientBuilder`].
//! - `HttpRequest` — wraps [`libdd_http_client::HttpRequest`].
//! - `HttpResponse` — wraps [`libdd_http_client::HttpResponse`].
//! - `HttpMethod` — enum mirroring [`libdd_http_client::HttpMethod`].
//! - `MultipartPart` — wraps [`libdd_http_client::MultipartPart`].
//! - `RetryConfig` — wraps [`libdd_http_client::RetryConfig`].
//! - `SharedRuntime` — wraps `Arc<libdd_shared_runtime::SharedRuntime>`. See
//!   the runtime module's docstring for why the wrapper lives in this crate
//!   instead of `libdd-shared-runtime-ffi`.
//! - Agent-client classes — `AgentClient`, `AgentClientBuilder`,
//!   `LanguageMetadata`, `AgentTransport`, `TraceFormat`, `TraceSendOptions`,
//!   `AgentResponse`, `TelemetryRequest`, `AgentInfo`.
//!
//! Python exceptions exposed:
//!
//! - `HttpClientError` — base class for `libdd_http_client::HttpClientError`.
//! - `ConnectionFailedError`, `TimedOutError`, `RequestFailedError`,
//!   `InvalidConfigError`, `IoError`.
//! - `AgentClientError` — separate base class for `libdd_agent_client` errors.
//! - `AgentBuildError`, `AgentTransportError`, `AgentHttpError`,
//!   `AgentRetriesExhaustedError`, `AgentEncodingError`.
//!
//! The two hierarchies are separate roots because the underlying Rust error
//! types are independent — a caller catching `HttpClientError` should not
//! incidentally catch agent-specific errors and vice versa.
//!
//! # Header round-trip
//!
//! Headers are exchanged as `dict[str, str]` on the Python side and stored as
//! `Vec<(String, String)>` on the Rust side. The dict→vec direction loses
//! ordering and de-duplicates by name (last write wins, per Python dict
//! semantics); the vec→dict direction preserves the last value for each
//! header. Bodies and header strings are UTF-8 by contract — non-UTF-8 input
//! raises `UnicodeDecodeError` from Python's str codec.

mod agent_client;
mod errors;
mod multipart;
mod request;
mod response;
mod retry;
mod runtime;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::time::Duration;

pub use agent_client::{
    map_build_error, map_send_error, AgentBuildError, AgentClient, AgentClientBuilder,
    AgentClientError, AgentEncodingError, AgentHttpError, AgentInfo, AgentResponse,
    AgentRetriesExhaustedError, AgentTransport, AgentTransportError, LanguageMetadata,
    TelemetryRequest, TraceFormat, TraceSendOptions,
};
pub use errors::{
    map_http_client_error, ConnectionFailedError, HttpClientError, InvalidConfigError, IoError,
    RequestFailedError, TimedOutError,
};
pub use multipart::MultipartPart;
pub use request::{HttpMethod, HttpRequest};
pub use response::HttpResponse;
pub use retry::RetryConfig;
pub use runtime::SharedRuntime;

/// Wraps [`libdd_http_client::HttpClient`].
#[pyclass(name = "HttpClient", module = "libdd_http_client")]
pub struct HttpClient {
    inner: libdd_http_client::HttpClient,
}

#[pymethods]
impl HttpClient {
    /// Construct a client with the given base URL and timeout (seconds).
    ///
    /// Mirrors [`libdd_http_client::HttpClient::new`]. For platform transports
    /// (Unix sockets, Windows named pipes) or `allow_connection_pooling`, use
    /// [`HttpClientBuilder`] instead.
    #[new]
    #[pyo3(signature = (base_url, timeout_secs))]
    pub fn new(base_url: String, timeout_secs: f64) -> PyResult<Self> {
        let timeout = duration_from_secs(timeout_secs)?;
        let inner = libdd_http_client::HttpClient::new(base_url, timeout)
            .map_err(map_http_client_error)?;
        Ok(Self { inner })
    }

    /// Send the given request synchronously, driving the future on the given
    /// shared runtime. Maps directly to
    /// [`libdd_http_client::HttpClient::send_blocking`].
    ///
    /// Releases the GIL for the duration of the I/O via `Python::detach` —
    /// otherwise the calling Python thread blocks every other Python thread
    /// (including any in-process mock servers used in smoke tests) while
    /// the request is in flight.
    pub fn send_blocking(
        &self,
        py: Python<'_>,
        request: &HttpRequest,
        runtime: &SharedRuntime,
    ) -> PyResult<HttpResponse> {
        let req = request.to_inner();
        let inner = &self.inner;
        let runtime_ref = runtime.as_inner();
        let result = py.detach(|| inner.send_blocking(req, runtime_ref));
        let resp = result.map_err(map_http_client_error)?;
        Ok(HttpResponse::from_inner(resp))
    }

    fn __repr__(&self) -> String {
        format!(
            "HttpClient(base_url={:?}, timeout_secs={:.6})",
            self.inner.config().base_url(),
            self.inner.config().timeout().as_secs_f64()
        )
    }
}

/// Wraps [`libdd_http_client::HttpClientBuilder`].
///
/// Builders are mutable and chained via mutating setters rather than
/// consuming `self`, since pyo3 borrowed references cannot return owned `Self`.
#[pyclass(name = "HttpClientBuilder", module = "libdd_http_client")]
#[derive(Default)]
pub struct HttpClientBuilder {
    base_url: Option<String>,
    timeout: Option<Duration>,
    allow_connection_pooling: bool,
    transport: TransportConfig,
    retry: Option<libdd_http_client::RetryConfig>,
}

#[derive(Debug, Clone, Default)]
enum TransportConfig {
    #[default]
    Tcp,
    #[cfg(unix)]
    UnixSocket(std::path::PathBuf),
    #[cfg(windows)]
    WindowsNamedPipe(std::ffi::OsString),
}

#[pymethods]
impl HttpClientBuilder {
    /// Construct a fresh builder with library defaults.
    #[new]
    pub fn new() -> Self {
        Self {
            base_url: None,
            timeout: None,
            // Match libdd-http-client's default.
            allow_connection_pooling: true,
            transport: TransportConfig::Tcp,
            retry: None,
        }
    }

    /// Set the base URL.
    fn set_base_url(&mut self, url: String) {
        self.base_url = Some(url);
    }

    /// Set the request timeout in seconds.
    fn set_timeout_secs(&mut self, secs: f64) -> PyResult<()> {
        self.timeout = Some(duration_from_secs(secs)?);
        Ok(())
    }

    /// Configure whether the underlying backend may pool connections.
    /// Defaults to `True` (matches [`libdd_http_client::HttpClientBuilder`]).
    fn set_allow_connection_pooling(&mut self, allow: bool) {
        self.allow_connection_pooling = allow;
    }

    /// Route all connections through the given Unix Domain Socket. Unix only.
    #[cfg(unix)]
    fn set_unix_socket(&mut self, path: String) {
        self.transport = TransportConfig::UnixSocket(path.into());
    }

    /// Route all connections through the given Windows Named Pipe. Windows only.
    #[cfg(windows)]
    fn set_windows_named_pipe(&mut self, pipe: String) {
        self.transport = TransportConfig::WindowsNamedPipe(pipe.into());
    }

    /// Enable automatic retries with the given [`RetryConfig`].
    ///
    /// Retries are off by default. Pass a configured `RetryConfig` to opt
    /// in; mirrors [`libdd_http_client::HttpClientBuilder::retry`].
    fn retry(&mut self, config: &RetryConfig) {
        self.retry = Some(config.to_inner());
    }

    /// Consume the builder and produce an [`HttpClient`].
    fn build(&mut self) -> PyResult<HttpClient> {
        let base_url = self.base_url.clone().ok_or_else(|| {
            InvalidConfigError::new_err("base_url is required".to_owned())
        })?;
        let timeout = self
            .timeout
            .ok_or_else(|| InvalidConfigError::new_err("timeout is required".to_owned()))?;

        let mut builder = libdd_http_client::HttpClient::builder()
            .base_url(base_url)
            .timeout(timeout)
            .allow_connection_pooling(self.allow_connection_pooling);

        if let Some(retry) = self.retry.clone() {
            builder = builder.retry(retry);
        }

        match &self.transport {
            TransportConfig::Tcp => {}
            #[cfg(unix)]
            TransportConfig::UnixSocket(path) => {
                builder = builder.unix_socket(path.clone());
            }
            #[cfg(windows)]
            TransportConfig::WindowsNamedPipe(pipe) => {
                builder = builder.windows_named_pipe(pipe.clone());
            }
        }

        let client = builder.build().map_err(map_http_client_error)?;
        Ok(HttpClient { inner: client })
    }
}

fn duration_from_secs(secs: f64) -> PyResult<Duration> {
    if !secs.is_finite() || secs < 0.0 {
        return Err(InvalidConfigError::new_err(format!(
            "timeout must be a non-negative finite number, got {secs}"
        )));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Install the FIPS-compliant TLS crypto provider.
///
/// Wraps [`libdd_http_client::init_fips_crypto`]. Only available when
/// compiled with the `fips` feature. Call once at process start, before
/// constructing any `HttpClient` that issues HTTPS requests. Returns an
/// error if a crypto provider is already installed.
#[cfg(feature = "fips")]
#[pyfunction]
fn init_fips_crypto() -> PyResult<()> {
    libdd_http_client::init_fips_crypto().map_err(map_http_client_error)
}

/// pyo3 module entry point.
///
/// Importing the wheel as `libdd_http_client` invokes this function and
/// populates the module with the classes and exceptions documented at the
/// crate root.
///
/// The function is named `init_module` to avoid colliding with the
/// `libdd_http_client` *crate* identifier; the Python-side module name is
/// driven by the `#[pyo3(name)]` attribute.
///
/// # Crypto provider
///
/// Without the `fips` feature, we install rustls' ring crypto provider as
/// the process-default. Because `libdd-http-client`'s reqwest backend uses
/// `rustls-no-provider`, *some* provider must be installed before the first
/// HTTPS connection. Installing ring here matches the test setup in the
/// underlying crate and avoids forcing every Python caller to learn about
/// rustls. The install is best-effort (`let _`): if a provider is already
/// installed (e.g. because the user explicitly called `init_fips_crypto`
/// first, or another library beat us to it), we do nothing.
///
/// With the `fips` feature, we do **not** install ring at init time — the
/// caller is expected to call [`init_fips_crypto`] explicitly. Doing both
/// would defeat the purpose of building with FIPS.
#[pymodule(name = "libdd_http_client")]
fn init_module(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    #[cfg(not(feature = "fips"))]
    {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    m.add_class::<HttpClient>()?;
    m.add_class::<HttpClientBuilder>()?;
    m.add_class::<HttpRequest>()?;
    m.add_class::<HttpResponse>()?;
    m.add_class::<HttpMethod>()?;
    m.add_class::<MultipartPart>()?;
    m.add_class::<RetryConfig>()?;
    m.add_class::<SharedRuntime>()?;

    m.add("HttpClientError", py.get_type::<HttpClientError>())?;
    m.add(
        "ConnectionFailedError",
        py.get_type::<ConnectionFailedError>(),
    )?;
    m.add("TimedOutError", py.get_type::<TimedOutError>())?;
    m.add("RequestFailedError", py.get_type::<RequestFailedError>())?;
    m.add("InvalidConfigError", py.get_type::<InvalidConfigError>())?;
    m.add("IoError", py.get_type::<IoError>())?;
    m.add(
        "SharedRuntimeError",
        py.get_type::<runtime::SharedRuntimeError>(),
    )?;

    #[cfg(feature = "fips")]
    m.add_function(wrap_pyfunction!(init_fips_crypto, m)?)?;

    agent_client::register(py, m)?;

    Ok(())
}

/// Convert a Python `dict[str, str]` into the Rust `Vec<(String, String)>`
/// shape used by [`libdd_http_client::HttpRequest`] and friends. Public so
/// other crates depending on this one can use the same conversion.
pub fn headers_from_pydict(dict: &Bound<'_, PyDict>) -> PyResult<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(dict.len());
    for (k, v) in dict.iter() {
        let name: String = k.extract()?;
        let value: String = v.extract()?;
        out.push((name, value));
    }
    Ok(out)
}
