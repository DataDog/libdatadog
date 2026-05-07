// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Python wrapper for [`libdd_http_client::HttpRequest`] and `HttpMethod`.
//!
//! Bodies are accepted as `bytes`; headers are accepted as `dict[str, str]`
//! with UTF-8 contracts. See the crate-root docstring for the round-trip
//! semantics.

use crate::multipart::MultipartPart;
use crate::{duration_from_secs, headers_from_pydict, InvalidConfigError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

/// Mirror of [`libdd_http_client::HttpMethod`] usable from Python.
///
/// `from_py_object` opts in to pyo3 0.28's `FromPyObject` derive — without it
/// pyo3 emits a deprecation warning saying the implicit derive on `Clone`
/// types is going away. Note that `eq_int` makes the enum comparable from
/// Python via `==`.
#[pyclass(
    name = "HttpMethod",
    module = "libdd_http_client",
    eq,
    eq_int,
    from_py_object
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// DELETE.
    Delete,
    /// HEAD.
    Head,
    /// PATCH.
    Patch,
    /// OPTIONS.
    Options,
}

#[pymethods]
impl HttpMethod {
    fn __repr__(&self) -> &'static str {
        match self {
            HttpMethod::Get => "HttpMethod.Get",
            HttpMethod::Post => "HttpMethod.Post",
            HttpMethod::Put => "HttpMethod.Put",
            HttpMethod::Delete => "HttpMethod.Delete",
            HttpMethod::Head => "HttpMethod.Head",
            HttpMethod::Patch => "HttpMethod.Patch",
            HttpMethod::Options => "HttpMethod.Options",
        }
    }
}

impl From<HttpMethod> for libdd_http_client::HttpMethod {
    fn from(m: HttpMethod) -> Self {
        match m {
            HttpMethod::Get => libdd_http_client::HttpMethod::Get,
            HttpMethod::Post => libdd_http_client::HttpMethod::Post,
            HttpMethod::Put => libdd_http_client::HttpMethod::Put,
            HttpMethod::Delete => libdd_http_client::HttpMethod::Delete,
            HttpMethod::Head => libdd_http_client::HttpMethod::Head,
            HttpMethod::Patch => libdd_http_client::HttpMethod::Patch,
            HttpMethod::Options => libdd_http_client::HttpMethod::Options,
        }
    }
}

impl From<libdd_http_client::HttpMethod> for HttpMethod {
    fn from(m: libdd_http_client::HttpMethod) -> Self {
        match m {
            libdd_http_client::HttpMethod::Get => HttpMethod::Get,
            libdd_http_client::HttpMethod::Post => HttpMethod::Post,
            libdd_http_client::HttpMethod::Put => HttpMethod::Put,
            libdd_http_client::HttpMethod::Delete => HttpMethod::Delete,
            libdd_http_client::HttpMethod::Head => HttpMethod::Head,
            libdd_http_client::HttpMethod::Patch => HttpMethod::Patch,
            libdd_http_client::HttpMethod::Options => HttpMethod::Options,
        }
    }
}

/// Python wrapper for [`libdd_http_client::HttpRequest`].
///
/// The Rust object is held by value and cloned on `to_inner()` — the wrapped
/// type is `Clone` and the bodies are `bytes::Bytes` (cheap reference-counted
/// clones), so this is fine for the typical "construct + send a few times"
/// pattern.
#[pyclass(name = "HttpRequest", module = "libdd_http_client", from_py_object)]
#[derive(Clone)]
pub struct HttpRequest {
    inner: libdd_http_client::HttpRequest,
}

#[pymethods]
impl HttpRequest {
    /// Construct a request.
    ///
    /// `headers` is `dict[str, str]`; `body` is `bytes`. Both are optional.
    /// `timeout_secs` overrides the client-level timeout for this request.
    #[new]
    #[pyo3(signature = (method, url, headers=None, body=None, timeout_secs=None))]
    pub fn new(
        method: HttpMethod,
        url: String,
        headers: Option<&Bound<'_, PyDict>>,
        body: Option<Vec<u8>>,
        timeout_secs: Option<f64>,
    ) -> PyResult<Self> {
        let mut req = libdd_http_client::HttpRequest::new(method.into(), url);
        if let Some(headers) = headers {
            for (name, value) in headers_from_pydict(headers)? {
                req = req.with_header(name, value);
            }
        }
        if let Some(body) = body {
            req = req.with_body(body);
        }
        if let Some(secs) = timeout_secs {
            req = req.with_timeout(duration_from_secs(secs)?);
        }
        Ok(Self { inner: req })
    }

    #[getter]
    fn method(&self) -> HttpMethod {
        self.inner.method().into()
    }

    #[getter]
    fn url(&self) -> &str {
        self.inner.url()
    }

    /// Returns the headers as a `dict[str, str]`. Last write wins per name.
    #[getter]
    fn headers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (name, value) in self.inner.headers() {
            dict.set_item(name, value)?;
        }
        Ok(dict)
    }

    /// Returns the body as `bytes`.
    #[getter]
    fn body<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.body())
    }

    /// Append a single header. Useful when you want to preserve duplicate
    /// header names (e.g. `Set-Cookie`-style multi-value headers).
    fn add_header(&mut self, name: String, value: String) {
        self.inner.headers_mut().push((name, value));
    }

    /// Replace the body. Accepts `bytes`.
    fn set_body(&mut self, body: Vec<u8>) {
        *self.inner.body_mut() = body.into();
    }

    /// Append a multipart part to this request.
    ///
    /// When the request has at least one multipart part attached, the
    /// underlying client sends it as `multipart/form-data` and the body bytes
    /// (if any) are ignored. Mirrors
    /// [`libdd_http_client::HttpRequest::with_multipart_part`].
    pub fn with_multipart_part(&mut self, part: &MultipartPart) {
        self.inner.multipart_parts_mut().push(part.to_inner());
    }

    /// Returns the number of multipart parts attached to this request. Useful
    /// for tests that want to verify the multipart path was taken.
    #[getter]
    pub fn multipart_parts_len(&self) -> usize {
        self.inner.multipart_parts().len()
    }

    /// Set the per-request timeout override (seconds).
    ///
    /// There is intentionally no "clear" path — once set, the request keeps
    /// the override. Construct a fresh `HttpRequest` if you need a different
    /// (or no) timeout.
    fn set_timeout_secs(&mut self, secs: f64) -> PyResult<()> {
        let d = duration_from_secs(secs)
            .map_err(|e| InvalidConfigError::new_err(e.to_string()))?;
        let inner = std::mem::replace(
            &mut self.inner,
            libdd_http_client::HttpRequest::new(
                libdd_http_client::HttpMethod::Get,
                String::new(),
            ),
        );
        self.inner = inner.with_timeout(d);
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "HttpRequest(method={:?}, url={:?}, body_len={})",
            self.inner.method(),
            self.inner.url(),
            self.inner.body().len()
        )
    }
}

impl HttpRequest {
    pub(crate) fn to_inner(&self) -> libdd_http_client::HttpRequest {
        self.inner.clone()
    }
}
