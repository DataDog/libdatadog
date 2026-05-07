// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Smoke tests for the agent-client surface through the pyo3 layer.
//!
//! Same shape as the other integration tests: drive the `#[pyclass]` types
//! from Rust against `httpmock`. Verifies:
//!
//! - `agent_info` returning `Some(AgentInfo)` round-trips the JSON config.
//! - `agent_info` returning `None` on 404.
//! - `send_traces` reaches the mock with the expected headers and body.

use httpmock::prelude::*;
use libdd_http_client_pyo3::{
    AgentClient, AgentClientBuilder, AgentTransport, LanguageMetadata, SharedRuntime, TraceFormat,
    TraceSendOptions,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;

fn ensure_python() {
    Python::initialize();
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn build_client(server: &MockServer) -> AgentClient {
    let mut builder = AgentClientBuilder::new();
    builder.set_transport(AgentTransport::http("127.0.0.1".to_owned(), server.port()));
    builder.set_language_metadata(LanguageMetadata::new(
        "python".to_owned(),
        "3.12".to_owned(),
        "CPython".to_owned(),
        "2.18.0".to_owned(),
    ));
    builder.set_timeout_millis(5_000);
    builder.build().expect("agent client build")
}

#[test]
fn agent_info_returns_some_with_parsed_config() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    let body = serde_json::json!({
        "version": "7.50.0",
        "endpoints": ["/v0.4/traces", "/v0.5/traces", "/info"],
        "client_drop_p0s": true,
        "config": {
            "default_env": "test",
            "feature_flags": ["foo", "bar"],
            "extra": { "nested": 42 }
        }
    });
    let mock = server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(200)
            .header("content-type", "application/json")
            .header("Datadog-Container-Tags-Hash", "deadbeef")
            .header("Datadog-Agent-State", "state-token-1")
            .body(body.to_string());
    });

    Python::attach(|py| {
        let client = Py::new(py, build_client(&server)).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();
        let info_opt: Option<_> = client
            .borrow(py)
            .agent_info(py, &runtime.borrow(py))
            .unwrap();
        let info = info_opt.expect("expected Some(AgentInfo)");
        let bound = Py::new(py, info).unwrap().into_pyobject(py).unwrap();

        let version: Option<String> = bound.getattr("version").unwrap().extract().unwrap();
        assert_eq!(version.as_deref(), Some("7.50.0"));

        let endpoints: Vec<String> = bound.getattr("endpoints").unwrap().extract().unwrap();
        assert_eq!(
            endpoints,
            vec![
                "/v0.4/traces".to_owned(),
                "/v0.5/traces".to_owned(),
                "/info".to_owned()
            ]
        );

        let drop_p0s: bool = bound.getattr("client_drop_p0s").unwrap().extract().unwrap();
        assert!(drop_p0s);

        let cth: Option<String> = bound
            .getattr("container_tags_hash")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(cth.as_deref(), Some("deadbeef"));

        let sh: Option<String> = bound.getattr("state_hash").unwrap().extract().unwrap();
        assert_eq!(sh.as_deref(), Some("state-token-1"));

        // pythonize: `config` should round-trip as a Python dict.
        let cfg = bound.getattr("config").unwrap();
        let cfg_dict: Bound<'_, PyDict> = cfg.cast::<PyDict>().unwrap().clone();
        let default_env: String = cfg_dict
            .get_item("default_env")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(default_env, "test");
        let feature_flags: Vec<String> = cfg_dict
            .get_item("feature_flags")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(feature_flags, vec!["foo".to_owned(), "bar".to_owned()]);
    });

    mock.assert();
}

#[test]
fn agent_info_returns_none_on_404() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET).path("/info");
        then.status(404).body("not found");
    });

    Python::attach(|py| {
        let client = Py::new(py, build_client(&server)).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();
        let info_opt = client
            .borrow(py)
            .agent_info(py, &runtime.borrow(py))
            .unwrap();
        assert!(info_opt.is_none(), "expected None on 404");
    });

    mock.assert();
}

#[test]
fn send_traces_blocking_smoke() {
    ensure_python();
    ensure_crypto_provider();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(PUT)
            .path("/v0.4/traces")
            .header("content-type", "application/msgpack")
            .header("X-Datadog-Trace-Count", "1")
            .header("Datadog-Send-Real-Http-Status", "true")
            .header("Datadog-Client-Computed-Top-Level", "yes");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"rate_by_service":{"service:env":0.5}}"#);
    });

    Python::attach(|py| {
        let client = Py::new(py, build_client(&server)).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let payload = vec![0x80u8, 0x81, 0x82, 0x83]; // arbitrary "msgpack-ish" bytes.
        let opts = TraceSendOptions::new(true);

        let resp = client
            .borrow(py)
            .send_traces(
                py,
                payload,
                1,
                TraceFormat::MsgpackV4,
                &opts,
                &runtime.borrow(py),
            )
            .expect("send_traces should succeed");
        let bound = Py::new(py, resp).unwrap().into_pyobject(py).unwrap();
        let status: u16 = bound.getattr("status").unwrap().extract().unwrap();
        assert_eq!(status, 200);
        // rate_by_service is parsed from the body.
        let rates_obj = bound.getattr("rate_by_service").unwrap();
        let rates: Bound<'_, PyDict> = rates_obj.cast::<PyDict>().unwrap().clone();
        let r: f64 = rates
            .get_item("service:env")
            .unwrap()
            .unwrap()
            .extract()
            .unwrap();
        assert!((r - 0.5).abs() < 1e-9);
    });

    mock.assert();
}

#[test]
fn agent_client_builder_missing_transport_errors() {
    ensure_python();
    Python::attach(|py| {
        let mut builder = AgentClientBuilder::new();
        builder.set_language_metadata(LanguageMetadata::new(
            "python".to_owned(),
            "3.12".to_owned(),
            "CPython".to_owned(),
            "2.18.0".to_owned(),
        ));
        match builder.build() {
            Ok(_) => panic!("expected AgentBuildError"),
            Err(err) => {
                assert!(err.is_instance_of::<libdd_http_client_pyo3::AgentBuildError>(py));
            }
        }
    });
}
