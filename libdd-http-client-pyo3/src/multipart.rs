// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Python wrapper for [`libdd_http_client::MultipartPart`].
//!
//! Multipart parts are constructed from Python `bytes` objects and then
//! attached to an [`crate::HttpRequest`] via
//! [`crate::HttpRequest::with_multipart_part`]. The Rust side stores the data
//! as `bytes::Bytes`, which is reference-counted, so cloning a `MultipartPart`
//! to attach it to a request is cheap.
//!
//! # File streaming
//!
//! "File streaming" here means: reading the whole file in Python (e.g.
//! `pathlib.Path("blob").read_bytes()`) and passing the resulting `bytes`
//! object to [`MultipartPart::new`]. We do **not** stream a file descriptor
//! through pyo3 — the underlying `bytes::Bytes` is a contiguous in-memory
//! buffer. For very large uploads, a future patch may add a chunked path.

use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// Python wrapper for [`libdd_http_client::MultipartPart`].
///
/// Constructed once and attached to one or more
/// [`crate::HttpRequest`] instances via
/// [`crate::HttpRequest::with_multipart_part`]. The wrapper is `Clone` because
/// the underlying `Bytes` is reference-counted; cloning is cheap and does not
/// duplicate the buffer.
#[pyclass(name = "MultipartPart", module = "libdd_http_client", from_py_object)]
#[derive(Clone)]
pub struct MultipartPart {
    inner: libdd_http_client::MultipartPart,
}

#[pymethods]
impl MultipartPart {
    /// Construct a multipart part with the given form-field `name` and binary
    /// `data` (Python `bytes`).
    ///
    /// `filename` and `content_type` are optional and may be added either via
    /// the keyword arguments here or by chaining
    /// [`MultipartPart::with_filename`] / [`MultipartPart::with_content_type`].
    ///
    /// The `bytes` object is converted to `bytes::Bytes` via a copy on
    /// construction. For zero-copy passthrough we would need to keep a
    /// reference to the Python buffer alive, which is harder under the abi3
    /// constraints we ship under; the copy is acceptable for typical
    /// multipart payload sizes.
    #[new]
    #[pyo3(signature = (name, data, filename=None, content_type=None))]
    pub fn new(
        name: String,
        data: Vec<u8>,
        filename: Option<String>,
        content_type: Option<String>,
    ) -> Self {
        let mut inner = libdd_http_client::MultipartPart::new(name, Bytes::from(data));
        if let Some(filename) = filename {
            inner = inner.with_filename(filename);
        }
        if let Some(ct) = content_type {
            inner = inner.with_content_type(ct);
        }
        Self { inner }
    }

    /// Returns a new [`MultipartPart`] with the `filename` attribute set.
    ///
    /// Mirrors [`libdd_http_client::MultipartPart::with_filename`].
    fn with_filename(&self, filename: String) -> Self {
        Self {
            inner: self.inner.clone().with_filename(filename),
        }
    }

    /// Returns a new [`MultipartPart`] with the `Content-Type` attribute set.
    ///
    /// Mirrors [`libdd_http_client::MultipartPart::with_content_type`].
    fn with_content_type(&self, content_type: String) -> Self {
        Self {
            inner: self.inner.clone().with_content_type(content_type),
        }
    }

    /// The form-field name.
    #[getter]
    fn name(&self) -> &str {
        self.inner.name()
    }

    /// The part's data as Python `bytes`.
    #[getter]
    fn data<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.inner.data())
    }

    /// The filename, if set, otherwise `None`.
    #[getter]
    fn filename(&self) -> Option<&str> {
        self.inner.filename()
    }

    /// The MIME content type, if set, otherwise `None`.
    #[getter]
    fn content_type(&self) -> Option<&str> {
        self.inner.content_type()
    }

    fn __repr__(&self) -> String {
        format!(
            "MultipartPart(name={:?}, data_len={}, filename={:?}, content_type={:?})",
            self.inner.name(),
            self.inner.data().len(),
            self.inner.filename(),
            self.inner.content_type(),
        )
    }
}

impl MultipartPart {
    /// Borrow the underlying [`libdd_http_client::MultipartPart`].
    pub(crate) fn to_inner(&self) -> libdd_http_client::MultipartPart {
        self.inner.clone()
    }
}
