// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Regression test that the agent `/info` fetch keeps opting into entity headers.
//!
//! Runs in its own test binary because `DD_EXTERNAL_ENV` is read once per process through a
//! `LazyLock` in `libdd-host-identity`, so it must be set before anything reads it.

use httpmock::prelude::*;
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use libdd_data_pipeline::agent_info::fetch_info;

#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn agent_info_request_carries_entity_headers() {
    // SAFETY: no other test runs in this binary.
    unsafe { std::env::set_var("DD_EXTERNAL_ENV", "it-false,cn-app,pu-1") };
    let server = MockServer::start();
    // The mock only matches requests carrying the header, so dropping
    // `.with_entity_headers()` from the fetcher fails this test.
    let mock = server.mock(|when, then| {
        when.path("/info")
            .header("datadog-external-env", "it-false,cn-app,pu-1");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"version":"entity-headers-test"}"#);
    });
    let endpoint = Endpoint::from_url(server.url("/info").parse().unwrap());
    let info = fetch_info::<NativeCapabilities>(&endpoint)
        .await
        .expect("fetch should succeed");
    mock.assert();
    assert_eq!(info.info.version.as_deref(), Some("entity-headers-test"));
}
