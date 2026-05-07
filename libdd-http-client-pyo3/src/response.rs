// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Python wrapper for [`libdd_http_client::HttpResponse`].

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

/// Python wrapper for [`libdd_http_client::HttpResponse`].
///
/// Constructed by [`crate::HttpClient::send_blocking`]; not intended to be
/// instantiated directly from Python.
#[pyclass(name = "HttpResponse", module = "libdd_http_client")]
#[derive(Debug)]
pub struct HttpResponse {
    inner: libdd_http_client::HttpResponse,
}

#[pymethods]
impl HttpResponse {
    /// HTTP status code (e.g. 200, 404, 503).
    #[getter]
    fn status_code(&self) -> u16 {
        self.inner.status_code()
    }

    /// Response headers as a `dict[str, str]`. Last value wins per name; if
    /// you need full duplicate-preserving access, use [`Self::headers_pairs`].
    #[getter]
    fn headers<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (name, value) in self.inner.headers() {
            dict.set_item(name, value)?;
        }
        Ok(dict)
    }

    /// Response headers as a list of `(name, value)` tuples. Preserves order
    /// and duplicates from the wire.
    fn headers_pairs(&self) -> Vec<(String, String)> {
        self.inner.headers().to_vec()
    }

    /// Response body as `bytes`.
    #[getter]
    fn body<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.body())
    }

    fn __repr__(&self) -> String {
        format!(
            "HttpResponse(status_code={}, body_len={})",
            self.inner.status_code(),
            self.inner.body().len()
        )
    }
}

impl HttpResponse {
    pub(crate) fn from_inner(inner: libdd_http_client::HttpResponse) -> Self {
        Self { inner }
    }
}
