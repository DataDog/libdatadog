// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end integration tests for the pyo3 layer.
//!
//! Drives the `#[pyclass]` types from Rust against a local mock server. This
//! mirrors what a real Python smoke test would do, just executed through
//! pyo3's Rust API surface so we can validate it without `maturin develop`.

use httpmock::prelude::*;
use libdd_http_client_pyo3::{
    map_http_client_error, ConnectionFailedError, HttpClient, HttpMethod, HttpRequest,
    HttpResponse, RequestFailedError, SharedRuntime,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;

fn ensure_python() {
    Python::initialize();
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[test]
fn smoke_get_against_mock_server() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/ping");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"ok":true}"#);
    });

    Python::attach(|py| {
        let client = Py::new(py, HttpClient::new(server.base_url(), 5.0).unwrap()).unwrap();
        let req = Py::new(
            py,
            HttpRequest::new(
                HttpMethod::Get,
                format!("{}/ping", server.base_url()),
                None,
                None,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let resp_obj: Py<HttpResponse> = client
            .borrow(py)
            .send_blocking(py, &req.borrow(py), &runtime.borrow(py))
            .map(|r| Py::new(py, r).unwrap())
            .unwrap();
        let resp = resp_obj.borrow(py);
        // Round-trip via the Python-visible getters too.
        let status = resp
            .into_pyobject(py)
            .unwrap()
            .getattr("status_code")
            .unwrap()
            .extract::<u16>()
            .unwrap();
        assert_eq!(status, 200);
    });

    mock.assert();
}

#[test]
fn smoke_post_json_against_mock_server() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v0.4/traces")
            .header("content-type", "application/json")
            .body(r#"{"hello":"world"}"#);
        then.status(200).body("ok");
    });

    Python::attach(|py| {
        let client = Py::new(py, HttpClient::new(server.base_url(), 5.0).unwrap()).unwrap();

        let headers = PyDict::new(py);
        headers.set_item("content-type", "application/json").unwrap();
        let req_inner = HttpRequest::new(
            HttpMethod::Post,
            format!("{}/v0.4/traces", server.base_url()),
            Some(&headers),
            Some(b"{\"hello\":\"world\"}".to_vec()),
            None,
        )
        .unwrap();
        let req = Py::new(py, req_inner).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let resp = client
            .borrow(py)
            .send_blocking(py, &req.borrow(py), &runtime.borrow(py))
            .unwrap();
        let resp_py = Py::new(py, resp).unwrap();
        let bound = resp_py.into_pyobject(py).unwrap();
        let status: u16 = bound.getattr("status_code").unwrap().extract().unwrap();
        let body: Vec<u8> = bound.getattr("body").unwrap().extract().unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"ok");
    });

    mock.assert();
}

#[test]
fn smoke_request_failed_carries_status_and_body() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/down");
        then.status(503).body("service unavailable");
    });

    Python::attach(|py| {
        let client = Py::new(py, HttpClient::new(server.base_url(), 5.0).unwrap()).unwrap();
        let req = HttpRequest::new(
            HttpMethod::Get,
            format!("{}/down", server.base_url()),
            None,
            None,
            None,
        )
        .unwrap();
        let req = Py::new(py, req).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let err = client
            .borrow(py)
            .send_blocking(py, &req.borrow(py), &runtime.borrow(py))
            .unwrap_err();
        // pyo3's PyErr keeps the Python instance — check we mapped to the
        // RequestFailedError class and that it carries the status/body.
        assert!(err.is_instance_of::<RequestFailedError>(py));
        let val = err.value(py);
        let status: u16 = val.getattr("status").unwrap().extract().unwrap();
        let body: Vec<u8> = val.getattr("body").unwrap().extract().unwrap();
        assert_eq!(status, 503);
        assert_eq!(body, b"service unavailable");
    });
}

#[test]
fn smoke_connection_failed_to_unreachable_port() {
    ensure_python();
    ensure_crypto_provider();
    Python::attach(|py| {
        // Use port 1 (typically privileged + nothing listening) for a
        // deterministic connection refusal. We don't care about the exact
        // exception subclass — `ConnectionFailedError` and `IoError` are both
        // valid here depending on the backend.
        let client = Py::new(
            py,
            HttpClient::new("http://127.0.0.1:1".to_owned(), 1.0).unwrap(),
        )
        .unwrap();
        let req = HttpRequest::new(
            HttpMethod::Get,
            "http://127.0.0.1:1/".to_owned(),
            None,
            None,
            None,
        )
        .unwrap();
        let req = Py::new(py, req).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let err = client
            .borrow(py)
            .send_blocking(py, &req.borrow(py), &runtime.borrow(py))
            .unwrap_err();
        // Either ConnectionFailedError or its base class — both fine.
        assert!(
            err.is_instance_of::<ConnectionFailedError>(py)
                || err.is_instance_of::<libdd_http_client_pyo3::HttpClientError>(py)
        );
    });
}

#[test]
fn header_round_trip_dict() {
    ensure_python();
    Python::attach(|py| {
        let headers = PyDict::new(py);
        headers.set_item("X-Custom", "value-1").unwrap();
        headers.set_item("DD-API-Key", "abc123").unwrap();

        let req = HttpRequest::new(
            HttpMethod::Get,
            "http://example.test/".to_owned(),
            Some(&headers),
            None,
            None,
        )
        .unwrap();
        // Exercise the Python-visible `headers` getter to confirm round-trip.
        let py_req = Py::new(py, req).unwrap();
        let bound = py_req.into_pyobject(py).unwrap();
        let h = bound.getattr("headers").unwrap();
        let h_dict: Bound<'_, PyDict> = h.cast::<PyDict>().unwrap().clone();
        let val: String = h_dict
            .get_item("X-Custom")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(val, "value-1");
        let val: String = h_dict
            .get_item("DD-API-Key")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(val, "abc123");
    });
}

/// Confirms `map_http_client_error` is reachable via the public re-export.
#[test]
fn public_error_mapping_is_reachable() {
    ensure_python();
    let err = map_http_client_error(libdd_http_client::HttpClientError::TimedOut);
    Python::attach(|py| {
        assert!(err.is_instance_of::<libdd_http_client_pyo3::TimedOutError>(py));
    });
}
