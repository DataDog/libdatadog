// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// pyo3's `create_exception!` macro generates a struct without doc-comments,
// the same as in `errors.rs`; document each exception in the module docstring
// instead.
#![allow(missing_docs)]

//! Python wrappers for the [`libdd_agent_client`] surface.
//!
//! Mirrors the structure of the core `HttpClient` bindings: every Rust type
//! is wrapped in a `#[pyclass]`, errors are mapped to a parallel Python
//! exception hierarchy rooted at `AgentClientError`, and the six
//! `send_*_blocking` methods on [`AgentClient`] are exposed verbatim.
//!
//! # GIL release
//!
//! Each blocking send releases the Python GIL via `Python::detach` while the
//! request is in flight. This lets other Python threads make progress during
//! network I/O — particularly important for the `httpmock` test helpers,
//! which run an `axum` server on a background thread and must service
//! requests while the test thread is blocked inside `send_*_blocking`.
//!
//! # `AgentInfo.config` JSON round-trip
//!
//! The Rust [`libdd_agent_client::AgentInfo::config`] is a `serde_json::Value`
//! — a recursive JSON tree. Rather than walk the tree manually and translate
//! each node to a Python `dict` / `list` / scalar, we use the
//! [`pythonize`] crate, which wires `serde_json::Value` directly into pyo3's
//! `IntoPyObject` machinery. This keeps the Rust → Python conversion exact
//! (including nested objects and arrays) without us writing a recursive
//! traversal.
//!
//! # Why we don't expose the async `send_*` methods
//!
//! Python's tokio bindings (`pyo3-async-runtimes`) are out of scope for Task
//! 8b. dd-trace-py is single-threaded under the GIL, so blocking is the
//! natural fit. If async becomes useful later, it can be layered on top of
//! the blocking surface without breaking the ABI.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use pythonize::pythonize;

use crate::retry::RetryConfig;
use crate::runtime::SharedRuntime;

create_exception!(libdd_http_client, AgentClientError, PyException);
create_exception!(
    libdd_http_client,
    AgentBuildError,
    AgentClientError
);
create_exception!(
    libdd_http_client,
    AgentTransportError,
    AgentClientError
);
create_exception!(libdd_http_client, AgentHttpError, AgentClientError);
create_exception!(
    libdd_http_client,
    AgentRetriesExhaustedError,
    AgentClientError
);
create_exception!(
    libdd_http_client,
    AgentEncodingError,
    AgentClientError
);

/// Map a [`libdd_agent_client::BuildError`] to its Python exception class.
pub fn map_build_error(err: libdd_agent_client::BuildError) -> PyErr {
    AgentBuildError::new_err(err.to_string())
}

/// Map a [`libdd_agent_client::SendError`] to its Python exception class.
///
/// `HttpError` carries `status` and `body` attributes mirroring the
/// `RequestFailedError` structure used by the core HTTP client bindings.
pub fn map_send_error(err: libdd_agent_client::SendError) -> PyErr {
    use libdd_agent_client::SendError as E;
    match err {
        E::Transport(io_err) => AgentTransportError::new_err(io_err.to_string()),
        E::HttpError { status, body } => Python::attach(|py| {
            let exc = AgentHttpError::new_err(format!(
                "HTTP error {status}: {body_len} bytes",
                body_len = body.len(),
            ));
            let bound = exc.value(py);
            if let Err(e) = bound.setattr("status", status) {
                return e;
            }
            if let Err(e) = bound.setattr("body", body.to_vec()) {
                return e;
            }
            exc
        }),
        E::RetriesExhausted { last_error } => {
            let inner = map_send_error(*last_error);
            // Wrap the underlying error into `AgentRetriesExhaustedError` —
            // expose the inner cause as a `cause` attribute.
            Python::attach(|py| {
                let exc = AgentRetriesExhaustedError::new_err(
                    "retries exhausted".to_owned(),
                );
                let bound = exc.value(py);
                let cause = inner.value(py);
                if let Err(e) = bound.setattr("cause", cause) {
                    return e;
                }
                exc
            })
        }
        E::Encoding(s) => AgentEncodingError::new_err(s),
    }
}

// -- LanguageMetadata --------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::LanguageMetadata`].
#[pyclass(
    name = "LanguageMetadata",
    module = "libdd_http_client",
    from_py_object
)]
#[derive(Clone)]
pub struct LanguageMetadata {
    inner: libdd_agent_client::LanguageMetadata,
}

#[pymethods]
impl LanguageMetadata {
    /// Construct a `LanguageMetadata`.
    #[new]
    pub fn new(
        language: String,
        language_version: String,
        interpreter: String,
        tracer_version: String,
    ) -> Self {
        Self {
            inner: libdd_agent_client::LanguageMetadata::new(
                language,
                language_version,
                interpreter,
                tracer_version,
            ),
        }
    }

    #[getter]
    fn language(&self) -> &str {
        &self.inner.language
    }

    #[getter]
    fn language_version(&self) -> &str {
        &self.inner.language_version
    }

    #[getter]
    fn interpreter(&self) -> &str {
        &self.inner.interpreter
    }

    #[getter]
    fn tracer_version(&self) -> &str {
        &self.inner.tracer_version
    }

    fn __repr__(&self) -> String {
        format!(
            "LanguageMetadata(language={:?}, language_version={:?}, interpreter={:?}, tracer_version={:?})",
            self.inner.language,
            self.inner.language_version,
            self.inner.interpreter,
            self.inner.tracer_version,
        )
    }
}

impl LanguageMetadata {
    pub(crate) fn to_inner(&self) -> libdd_agent_client::LanguageMetadata {
        self.inner.clone()
    }
}

// -- AgentTransport ---------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::AgentTransport`].
///
/// Construct via the static methods [`AgentTransport::http`],
/// [`AgentTransport::https`] (with the `https` feature),
/// [`AgentTransport::unix_socket`] (Unix), or
/// [`AgentTransport::named_pipe`] (Windows).
#[pyclass(
    name = "AgentTransport",
    module = "libdd_http_client",
    from_py_object
)]
#[derive(Clone)]
pub struct AgentTransport {
    inner: libdd_agent_client::AgentTransport,
}

#[pymethods]
impl AgentTransport {
    /// HTTP over TCP.
    #[staticmethod]
    pub fn http(host: String, port: u16) -> Self {
        Self {
            inner: libdd_agent_client::AgentTransport::Http { host, port },
        }
    }

    /// HTTPS over TCP. Only available when compiled with the `https` feature.
    #[cfg(feature = "https")]
    #[staticmethod]
    pub fn https(host: String, port: u16) -> Self {
        Self {
            inner: libdd_agent_client::AgentTransport::Https { host, port },
        }
    }

    /// Unix Domain Socket. Unix only.
    #[cfg(unix)]
    #[staticmethod]
    pub fn unix_socket(path: String) -> Self {
        Self {
            inner: libdd_agent_client::AgentTransport::UnixSocket {
                path: path.into(),
            },
        }
    }

    /// Windows Named Pipe. Windows only.
    #[cfg(windows)]
    #[staticmethod]
    pub fn named_pipe(path: String) -> Self {
        Self {
            inner: libdd_agent_client::AgentTransport::NamedPipe {
                path: path.into(),
            },
        }
    }

    fn __repr__(&self) -> String {
        format!("AgentTransport({:?})", self.inner)
    }
}

impl AgentTransport {
    fn to_inner(&self) -> libdd_agent_client::AgentTransport {
        self.inner.clone()
    }
}

// -- TraceFormat -------------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::TraceFormat`].
///
/// Mirrors the Rust enum variants (`MsgpackV5`, `MsgpackV4`). `eq_int`
/// makes the variants comparable from Python via `==`.
#[pyclass(
    name = "TraceFormat",
    module = "libdd_http_client",
    eq,
    eq_int,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFormat {
    /// `application/msgpack` to `/v0.5/traces`. Preferred format.
    MsgpackV5,
    /// `application/msgpack` to `/v0.4/traces`. Fallback for Windows / AppSec.
    MsgpackV4,
}

#[pymethods]
impl TraceFormat {
    fn __repr__(&self) -> &'static str {
        match self {
            TraceFormat::MsgpackV5 => "TraceFormat.MsgpackV5",
            TraceFormat::MsgpackV4 => "TraceFormat.MsgpackV4",
        }
    }
}

impl From<TraceFormat> for libdd_agent_client::TraceFormat {
    fn from(t: TraceFormat) -> Self {
        match t {
            TraceFormat::MsgpackV5 => libdd_agent_client::TraceFormat::MsgpackV5,
            TraceFormat::MsgpackV4 => libdd_agent_client::TraceFormat::MsgpackV4,
        }
    }
}

// -- TraceSendOptions --------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::TraceSendOptions`].
#[pyclass(
    name = "TraceSendOptions",
    module = "libdd_http_client",
    from_py_object
)]
#[derive(Clone)]
pub struct TraceSendOptions {
    inner: libdd_agent_client::TraceSendOptions,
}

#[pymethods]
impl TraceSendOptions {
    /// Construct a fresh `TraceSendOptions`.
    #[new]
    #[pyo3(signature = (computed_top_level=false))]
    pub fn new(computed_top_level: bool) -> Self {
        Self {
            inner: libdd_agent_client::TraceSendOptions {
                computed_top_level,
            },
        }
    }

    #[getter]
    fn computed_top_level(&self) -> bool {
        self.inner.computed_top_level
    }

    #[setter]
    fn set_computed_top_level(&mut self, v: bool) {
        self.inner.computed_top_level = v;
    }

    fn __repr__(&self) -> String {
        format!(
            "TraceSendOptions(computed_top_level={})",
            self.inner.computed_top_level
        )
    }
}

impl TraceSendOptions {
    fn to_inner(&self) -> libdd_agent_client::TraceSendOptions {
        self.inner.clone()
    }
}

// -- AgentResponse -----------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::AgentResponse`].
#[pyclass(name = "AgentResponse", module = "libdd_http_client")]
pub struct AgentResponse {
    inner: libdd_agent_client::AgentResponse,
}

#[pymethods]
impl AgentResponse {
    /// HTTP status code returned by the agent.
    #[getter]
    fn status(&self) -> u16 {
        self.inner.status
    }

    /// Per-service sampling rates parsed from the response body, if present.
    ///
    /// Returns `None` if the agent did not include a `rate_by_service` field.
    #[getter]
    fn rate_by_service<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        match self.inner.rate_by_service.as_ref() {
            None => Ok(None),
            Some(map) => {
                let dict = PyDict::new(py);
                for (k, v) in map.iter() {
                    dict.set_item(k, v)?;
                }
                Ok(Some(dict))
            }
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "AgentResponse(status={}, rate_by_service={})",
            self.inner.status,
            self.inner
                .rate_by_service
                .as_ref()
                .map(|m| format!("{} entries", m.len()))
                .unwrap_or_else(|| "None".to_owned()),
        )
    }
}

impl AgentResponse {
    fn from_inner(inner: libdd_agent_client::AgentResponse) -> Self {
        Self { inner }
    }
}

// -- TelemetryRequest --------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::TelemetryRequest`].
#[pyclass(
    name = "TelemetryRequest",
    module = "libdd_http_client",
    from_py_object
)]
#[derive(Clone)]
pub struct TelemetryRequest {
    inner: libdd_agent_client::TelemetryRequest,
}

#[pymethods]
impl TelemetryRequest {
    /// Construct a `TelemetryRequest`.
    ///
    /// `body` is a Python `bytes` containing the pre-serialised JSON payload.
    #[new]
    #[pyo3(signature = (request_type, api_version, body, debug=false))]
    pub fn new(
        request_type: String,
        api_version: String,
        body: Vec<u8>,
        debug: bool,
    ) -> Self {
        Self {
            inner: libdd_agent_client::TelemetryRequest {
                request_type,
                api_version,
                body: Bytes::from(body),
                debug,
            },
        }
    }

    #[getter]
    fn request_type(&self) -> &str {
        &self.inner.request_type
    }

    #[getter]
    fn api_version(&self) -> &str {
        &self.inner.api_version
    }

    #[getter]
    fn debug(&self) -> bool {
        self.inner.debug
    }

    #[getter]
    fn body<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.inner.body)
    }

    fn __repr__(&self) -> String {
        format!(
            "TelemetryRequest(request_type={:?}, api_version={:?}, debug={}, body_len={})",
            self.inner.request_type,
            self.inner.api_version,
            self.inner.debug,
            self.inner.body.len(),
        )
    }
}

impl TelemetryRequest {
    fn to_inner(&self) -> libdd_agent_client::TelemetryRequest {
        self.inner.clone()
    }
}

// -- AgentInfo ---------------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::AgentInfo`].
///
/// `config` is the parsed `/info` config block — round-tripped from
/// `serde_json::Value` via [`pythonize`] into native Python `dict` /
/// `list` / scalar types. `endpoints` is exposed as a Python `list[str]`.
///
/// `AgentInfo` is constructed only by [`AgentClient::agent_info`]; Python
/// callers cannot construct one or pass it back into the API. We therefore
/// opt out of `FromPyObject` (`skip_from_py_object`).
#[pyclass(
    name = "AgentInfo",
    module = "libdd_http_client",
    skip_from_py_object
)]
#[derive(Clone)]
pub struct AgentInfo {
    inner: libdd_agent_client::AgentInfo,
}

#[pymethods]
impl AgentInfo {
    /// Available agent endpoints, e.g. `["/v0.4/traces", "/v0.5/traces"]`.
    #[getter]
    fn endpoints<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        PyList::new(py, &self.inner.endpoints)
    }

    /// Whether the agent supports client-side P0 dropping.
    #[getter]
    fn client_drop_p0s(&self) -> bool {
        self.inner.client_drop_p0s
    }

    /// Raw agent configuration as a Python object — `dict` / `list` / scalars.
    /// Returns `None` if the underlying value is JSON `null`.
    #[getter]
    fn config<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // pythonize converts `serde_json::Value::Null` to Python's `None`,
        // so callers get a faithful round-trip without us special-casing it.
        pythonize(py, &self.inner.config)
            .map_err(|e| AgentEncodingError::new_err(format!("config pythonize: {e}")))
    }

    /// Agent version string, if reported.
    #[getter]
    fn version(&self) -> Option<&str> {
        self.inner.version.as_deref()
    }

    /// `Datadog-Container-Tags-Hash` response header value.
    #[getter]
    fn container_tags_hash(&self) -> Option<&str> {
        self.inner.container_tags_hash.as_deref()
    }

    /// `Datadog-Agent-State` response header value.
    #[getter]
    fn state_hash(&self) -> Option<&str> {
        self.inner.state_hash.as_deref()
    }

    fn __repr__(&self) -> String {
        format!(
            "AgentInfo(version={:?}, endpoints={} entries, client_drop_p0s={})",
            self.inner.version,
            self.inner.endpoints.len(),
            self.inner.client_drop_p0s,
        )
    }
}

impl AgentInfo {
    fn from_inner(inner: libdd_agent_client::AgentInfo) -> Self {
        Self { inner }
    }
}

// -- AgentClientBuilder ------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::AgentClientBuilder`].
///
/// The Rust builder consumes `self` on every setter, which is incompatible
/// with pyo3's borrowed-`&mut self` shape. We mirror what
/// [`crate::HttpClientBuilder`] does: hold the configuration as plain fields,
/// then translate to the underlying builder in [`AgentClientBuilder::build`].
#[pyclass(
    name = "AgentClientBuilder",
    module = "libdd_http_client"
)]
#[derive(Default)]
pub struct AgentClientBuilder {
    transport: Option<libdd_agent_client::AgentTransport>,
    test_token: Option<String>,
    timeout_ms: Option<u64>,
    language: Option<libdd_agent_client::LanguageMetadata>,
    retry: Option<libdd_http_client::RetryConfig>,
    allow_connection_pooling: bool,
    extra_headers: Vec<(String, String)>,
}

#[pymethods]
impl AgentClientBuilder {
    /// Construct a fresh builder.
    #[new]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the transport. See [`AgentTransport`].
    pub fn set_transport(&mut self, transport: AgentTransport) {
        self.transport = Some(transport.to_inner());
    }

    /// Convenience: HTTP over TCP.
    fn http(&mut self, host: String, port: u16) {
        self.transport = Some(libdd_agent_client::AgentTransport::Http { host, port });
    }

    /// Convenience: HTTPS over TCP.
    #[cfg(feature = "https")]
    fn https(&mut self, host: String, port: u16) {
        self.transport = Some(libdd_agent_client::AgentTransport::Https { host, port });
    }

    /// Convenience: Unix Domain Socket.
    #[cfg(unix)]
    fn unix_socket(&mut self, path: String) {
        self.transport = Some(libdd_agent_client::AgentTransport::UnixSocket {
            path: path.into(),
        });
    }

    /// Convenience: Windows Named Pipe.
    #[cfg(windows)]
    fn windows_named_pipe(&mut self, path: String) {
        self.transport = Some(libdd_agent_client::AgentTransport::NamedPipe {
            path: path.into(),
        });
    }

    /// Set the test session token.
    fn set_test_agent_session_token(&mut self, token: String) {
        self.test_token = Some(token);
    }

    /// Set the request timeout in milliseconds.
    pub fn set_timeout_millis(&mut self, ms: u64) {
        self.timeout_ms = Some(ms);
    }

    /// Set the language metadata. Required.
    pub fn set_language_metadata(&mut self, meta: LanguageMetadata) {
        self.language = Some(meta.to_inner());
    }

    /// Override the retry configuration.
    fn set_retry(&mut self, config: RetryConfig) {
        self.retry = Some(config.to_inner());
    }

    /// Allow connection pooling. Defaults to `False`.
    fn set_allow_connection_pooling(&mut self, allow: bool) {
        self.allow_connection_pooling = allow;
    }

    /// Append a custom header. Repeat to add multiple.
    fn add_extra_header(&mut self, name: String, value: String) {
        self.extra_headers.push((name, value));
    }

    /// Build the [`AgentClient`].
    ///
    /// Returns [`AgentBuildError`] on missing required fields or HTTP-client
    /// construction failure.
    pub fn build(&mut self) -> PyResult<AgentClient> {
        let mut builder = libdd_agent_client::AgentClientBuilder::new();

        let transport = self.transport.clone().ok_or_else(|| {
            AgentBuildError::new_err("transport is required".to_owned())
        })?;
        builder = builder.transport(transport);

        let language = self.language.clone().ok_or_else(|| {
            AgentBuildError::new_err("language metadata is required".to_owned())
        })?;
        builder = builder.language_metadata(language);

        if let Some(token) = self.test_token.clone() {
            builder = builder.test_agent_session_token(token);
        }
        if let Some(ms) = self.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms));
        }
        if let Some(retry) = self.retry.clone() {
            builder = builder.retry(retry);
        }
        builder = builder.allow_connection_pooling(self.allow_connection_pooling);
        if !self.extra_headers.is_empty() {
            builder = builder.extra_headers(self.extra_headers.clone());
        }

        let client = builder.build().map_err(map_build_error)?;
        Ok(AgentClient {
            inner: Arc::new(client),
        })
    }
}

// -- AgentClient -------------------------------------------------------------

/// Python wrapper for [`libdd_agent_client::AgentClient`].
///
/// The inner [`libdd_agent_client::AgentClient`] is held in an `Arc` so that
/// the pyclass type is `Send + Sync`, satisfying pyo3's `MaybeSend` bound.
/// `AgentClient` itself is not `Clone`; the `Arc` lets us hand out a borrow
/// from `Python::detach` without moving the value.
#[pyclass(name = "AgentClient", module = "libdd_http_client")]
pub struct AgentClient {
    inner: Arc<libdd_agent_client::AgentClient>,
}

#[pymethods]
impl AgentClient {
    /// Construct an [`AgentClientBuilder`].
    #[staticmethod]
    fn builder() -> AgentClientBuilder {
        AgentClientBuilder::new()
    }

    /// Send a serialised trace payload synchronously.
    ///
    /// Mirrors [`libdd_agent_client::AgentClient::send_traces_blocking`].
    pub fn send_traces(
        &self,
        py: Python<'_>,
        payload: Vec<u8>,
        trace_count: usize,
        format: TraceFormat,
        opts: &TraceSendOptions,
        runtime: &SharedRuntime,
    ) -> PyResult<AgentResponse> {
        let payload = Bytes::from(payload);
        let opts = opts.to_inner();
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result = py.detach(move || {
            inner.send_traces_blocking(payload, trace_count, format.into(), opts, &runtime_arc)
        });
        let resp = result.map_err(map_send_error)?;
        Ok(AgentResponse::from_inner(resp))
    }

    /// Send span stats synchronously.
    pub fn send_stats(
        &self,
        py: Python<'_>,
        payload: Vec<u8>,
        runtime: &SharedRuntime,
    ) -> PyResult<()> {
        let payload = Bytes::from(payload);
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result = py.detach(move || inner.send_stats_blocking(payload, &runtime_arc));
        result.map_err(map_send_error)
    }

    /// Send pipeline stats synchronously.
    pub fn send_pipeline_stats(
        &self,
        py: Python<'_>,
        payload: Vec<u8>,
        runtime: &SharedRuntime,
    ) -> PyResult<()> {
        let payload = Bytes::from(payload);
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result =
            py.detach(move || inner.send_pipeline_stats_blocking(payload, &runtime_arc));
        result.map_err(map_send_error)
    }

    /// Send a telemetry event synchronously.
    pub fn send_telemetry(
        &self,
        py: Python<'_>,
        request: &TelemetryRequest,
        runtime: &SharedRuntime,
    ) -> PyResult<()> {
        let req = request.to_inner();
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result = py.detach(move || inner.send_telemetry_blocking(req, &runtime_arc));
        result.map_err(map_send_error)
    }

    /// Send an EVP-proxy event synchronously.
    pub fn send_evp_event(
        &self,
        py: Python<'_>,
        subdomain: String,
        path: String,
        payload: Vec<u8>,
        content_type: String,
        runtime: &SharedRuntime,
    ) -> PyResult<()> {
        let payload = Bytes::from(payload);
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result = py.detach(move || {
            inner.send_evp_event_blocking(
                &subdomain,
                &path,
                payload,
                &content_type,
                &runtime_arc,
            )
        });
        result.map_err(map_send_error)
    }

    /// Probe `GET /info` synchronously.
    ///
    /// Returns `None` when the agent returns 404 (info not supported), or
    /// an [`AgentInfo`] otherwise.
    pub fn agent_info(
        &self,
        py: Python<'_>,
        runtime: &SharedRuntime,
    ) -> PyResult<Option<AgentInfo>> {
        let inner = self.inner.clone();
        let runtime_arc = runtime.clone_arc();
        let result = py.detach(move || inner.agent_info_blocking(&runtime_arc));
        let info = result.map_err(map_send_error)?;
        Ok(info.map(AgentInfo::from_inner))
    }
}

/// Register every Python class and exception in this module on `m`.
///
/// Called from the top-level `init_module` to keep
/// [`crate::init_module`] readable.
pub(crate) fn register(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<LanguageMetadata>()?;
    m.add_class::<AgentTransport>()?;
    m.add_class::<TraceFormat>()?;
    m.add_class::<TraceSendOptions>()?;
    m.add_class::<AgentResponse>()?;
    m.add_class::<TelemetryRequest>()?;
    m.add_class::<AgentInfo>()?;
    m.add_class::<AgentClientBuilder>()?;
    m.add_class::<AgentClient>()?;

    m.add("AgentClientError", py.get_type::<AgentClientError>())?;
    m.add("AgentBuildError", py.get_type::<AgentBuildError>())?;
    m.add(
        "AgentTransportError",
        py.get_type::<AgentTransportError>(),
    )?;
    m.add("AgentHttpError", py.get_type::<AgentHttpError>())?;
    m.add(
        "AgentRetriesExhaustedError",
        py.get_type::<AgentRetriesExhaustedError>(),
    )?;
    m.add(
        "AgentEncodingError",
        py.get_type::<AgentEncodingError>(),
    )?;
    Ok(())
}
