// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regression test that the telemetry request builder keeps opting into entity headers.
//!
//! Runs in its own test binary because `DD_EXTERNAL_ENV` is read once per process through a
//! `LazyLock` in `libdd-host-identity`, so it must be set before anything reads it.

use libdd_telemetry::config::Config;
use libdd_telemetry::worker::http_client::request_builder;

#[test]
fn telemetry_request_carries_entity_headers() {
    // SAFETY: no other test runs in this binary.
    unsafe { std::env::set_var("DD_EXTERNAL_ENV", "it-false,cn-app,pu-1") };
    let mut config = Config::default();
    config
        .set_endpoint_uri(http::Uri::from_static("http://localhost:8126"))
        .expect("valid endpoint");
    let request = request_builder(&config)
        .expect("request builder")
        .body(())
        .expect("request");
    assert_eq!(
        request.headers().get("datadog-external-env").unwrap(),
        "it-false,cn-app,pu-1"
    );
}
