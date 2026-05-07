// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for multipart upload through the pyo3 layer.
//!
//! Mirrors the existing integration tests' shape: drive the `#[pyclass]` types
//! from Rust against an `httpmock` server, asserting the request was received
//! with `multipart/form-data` and the expected file body. We don't attempt to
//! re-parse the wire body — `httpmock` accepts any body when we don't assert
//! on it, and what we care about is "did pyo3 hand the multipart parts off to
//! the underlying client?".

use httpmock::prelude::*;
use libdd_http_client_pyo3::{
    HttpClient, HttpMethod, HttpRequest, MultipartPart, SharedRuntime,
};
use pyo3::prelude::*;

fn ensure_python() {
    Python::initialize();
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[test]
fn multipart_part_basic_construction() {
    ensure_python();
    Python::attach(|py| {
        let part = Py::new(
            py,
            MultipartPart::new(
                "file".to_owned(),
                b"hello world".to_vec(),
                Some("blob.bin".to_owned()),
                Some("application/octet-stream".to_owned()),
            ),
        )
        .unwrap();
        let bound = part.into_pyobject(py).unwrap();
        let name: String = bound.getattr("name").unwrap().extract().unwrap();
        let data: Vec<u8> = bound.getattr("data").unwrap().extract().unwrap();
        let filename: Option<String> = bound.getattr("filename").unwrap().extract().unwrap();
        let content_type: Option<String> =
            bound.getattr("content_type").unwrap().extract().unwrap();
        assert_eq!(name, "file");
        assert_eq!(data, b"hello world");
        assert_eq!(filename.as_deref(), Some("blob.bin"));
        assert_eq!(content_type.as_deref(), Some("application/octet-stream"));
    });
}

#[test]
fn multipart_part_with_filename_and_content_type_chain() {
    ensure_python();
    Python::attach(|py| {
        let part = MultipartPart::new("upload".to_owned(), b"abc".to_vec(), None, None);
        let part = part.into_pyobject(py).unwrap();
        let part_with_filename = part
            .call_method1("with_filename", ("data.txt",))
            .unwrap();
        let part_with_ct = part_with_filename
            .call_method1("with_content_type", ("text/plain",))
            .unwrap();
        let filename: Option<String> = part_with_ct.getattr("filename").unwrap().extract().unwrap();
        let ct: Option<String> = part_with_ct.getattr("content_type").unwrap().extract().unwrap();
        assert_eq!(filename.as_deref(), Some("data.txt"));
        assert_eq!(ct.as_deref(), Some("text/plain"));
    });
}

#[test]
fn multipart_upload_smoke_against_mock_server() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    // The reqwest backend produces a `multipart/form-data; boundary=...`
    // content-type. We assert the prefix only.
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/upload")
            .header_matches("content-type", r"multipart/form-data; boundary=.*");
        then.status(200).body("uploaded");
    });

    Python::attach(|py| {
        let client =
            Py::new(py, HttpClient::new(server.base_url(), 5.0).unwrap()).unwrap();

        let mut req = HttpRequest::new(
            HttpMethod::Post,
            format!("{}/upload", server.base_url()),
            None,
            None,
            None,
        )
        .unwrap();
        let part = MultipartPart::new(
            "file".to_owned(),
            b"file content here".to_vec(),
            Some("blob.bin".to_owned()),
            Some("application/octet-stream".to_owned()),
        );
        req.with_multipart_part(&part);
        // Sanity-check the part was attached on the Rust side.
        let parts_len = req.multipart_parts_len();
        assert_eq!(parts_len, 1);

        let req = Py::new(py, req).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let resp = client
            .borrow(py)
            .send_blocking(py, &req.borrow(py), &runtime.borrow(py))
            .unwrap();
        let py_resp = Py::new(py, resp).unwrap();
        let bound = py_resp.into_pyobject(py).unwrap();
        let status: u16 = bound.getattr("status_code").unwrap().extract().unwrap();
        let body: Vec<u8> = bound.getattr("body").unwrap().extract().unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"uploaded");
    });

    mock.assert();
}
