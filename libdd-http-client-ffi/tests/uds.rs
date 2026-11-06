// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

mod common;

use common::*;
use libdd_http_client_ffi::*;

#[test]
fn requests_go_through_the_unix_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("agent.sock");
    let server = spawn_uds(&socket, respond_ok);
    let client = Cfg::new("http://localhost")
        .unix_socket(&socket)
        .build()
        .expect("client");

    let response = client
        .send(
            &Req::post("/v0.4/traces")
                .header("X-Test", "uds")
                .body(b"payload".to_vec()),
        )
        .expect("send");

    assert_eq!(response.status(), 200);
    assert_eq!(response.body(), b"ok");
    let received = server.recv();
    assert_eq!(received.method, "POST");
    assert_eq!(received.target, "/v0.4/traces");
    assert_eq!(received.header("x-test"), Some("uds"));
    assert_eq!(received.body, b"payload");
}

#[test]
fn the_host_of_the_base_url_is_ignored_with_a_unix_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("agent.sock");
    let server = spawn_uds(&socket, respond_ok);
    let client = Cfg::new("http://this-host-does-not-exist.invalid:1234")
        .unix_socket(&socket)
        .build()
        .expect("client");

    let response = client.send(&Req::get("/info")).expect("send");

    assert_eq!(response.status(), 200);
    assert_eq!(server.recv().target, "/info");
}

#[test]
fn a_missing_socket_is_connection_failed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = Cfg::new("http://localhost")
        .unix_socket(&dir.path().join("absent.sock"))
        .build()
        .expect("client");

    let error = client.send(&Req::get("/info")).err().expect("must fail");

    assert_eq!(
        error.code,
        HttpClientErrorCode::ConnectionFailed,
        "{}",
        error.message
    );
}
