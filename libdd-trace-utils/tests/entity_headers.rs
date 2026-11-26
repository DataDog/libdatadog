// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regression test that `send_with_retry` keeps opting into entity headers.
//!
//! Runs in its own test binary because `DD_EXTERNAL_ENV` is read once per process through a
//! `LazyLock` in `libdd-host-identity`, so it must be set before anything reads it.

use http::HeaderMap;
use libdd_capabilities::HttpClientCapability;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use libdd_trace_utils::send_with_retry::*;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn send_with_retry_carries_entity_headers() {
    // SAFETY: no other test runs in this binary.
    unsafe { std::env::set_var("DD_EXTERNAL_ENV", "it-false,cn-app,pu-1") };
    let server = httpmock::MockServer::start();
    // The mock only matches requests carrying the header, so dropping
    // `.with_entity_headers()` from the sender fails this test.
    let mock = server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .header("datadog-external-env", "it-false,cn-app,pu-1");
        then.status(202);
    });
    let endpoint = Endpoint::from_slice(&server.url("/v0.4/traces"));
    let strategy = RetryStrategy::new(3, 10, RetryBackoffType::Constant, None);
    let result = send_with_retry(
        &NativeCapabilities::new_client(),
        &endpoint,
        vec![1, 2, 3],
        &HeaderMap::new(),
        &strategy,
        CompressionStrategy::None,
    )
    .await;
    assert!(result.is_ok(), "expected a successful send: {result:?}");
    mock.assert();
}
