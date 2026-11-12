// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use common::*;
use httpmock::prelude::*;

#[test]
fn new_succeeds_for_a_tcp_config_without_connecting() {
    // Nothing listens here: building a client must not connect.
    let client = Cfg::for_addr(refused_addr()).build();
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn new_accepts_every_optional_setting() {
    let mut cfg = Cfg::new("http://localhost:8126")
        .default_header("Datadog-Meta-Lang", "ruby")
        .retries(3, 50);
    cfg.periodic = true;
    let client = cfg.build();
    assert!(client.is_ok(), "{:?}", client.err());
}

#[cfg(feature = "https")]
#[test]
fn new_succeeds_for_an_https_base_url_without_a_preinstalled_crypto_provider() {
    let client = Cfg::new("https://localhost:8443").build();
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn a_client_with_a_shared_runtime_keeps_working_after_the_caller_frees_its_reference() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });
    let runtime = new_shared_runtime();
    let client = Cfg::new(server.base_url())
        .build_with_runtime(runtime)
        .expect("client");
    unsafe { free_shared_runtime(runtime) };

    let response = client.send(&Req::get("/ping")).expect("send");
    assert_eq!(response.status(), 200);
}

#[test]
fn several_clients_can_share_one_runtime() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET);
        then.status(200);
    });
    let runtime = new_shared_runtime();
    let first = Cfg::new(server.base_url())
        .build_with_runtime(runtime)
        .expect("first client");
    let second = Cfg::new(server.base_url())
        .build_with_runtime(runtime)
        .expect("second client");
    unsafe { free_shared_runtime(runtime) };

    assert_eq!(first.send(&Req::get("/a")).expect("first").status(), 200);
    drop(first);
    // The second client must survive the first being dropped.
    assert_eq!(second.send(&Req::get("/b")).expect("second").status(), 200);
}
