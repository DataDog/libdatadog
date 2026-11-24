// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Without the `hyper-proxy` feature, an explicitly configured proxy must be
//! a build-time configuration error rather than a silently direct connection.
#![cfg(all(
    feature = "hyper-backend",
    not(feature = "reqwest-backend"),
    not(feature = "hyper-proxy")
))]

use libdd_http_client::{HttpClient, HttpClientError, ProxyConfig};
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider();
}

#[cfg_attr(miri, ignore)]
#[test]
fn proxy_config_without_hyper_proxy_feature_is_an_error() {
    ensure_crypto_provider();
    let client = HttpClient::builder()
        .base_url("http://localhost:8126".to_owned())
        .timeout(Duration::from_secs(5))
        .proxy(ProxyConfig::all("http://127.0.0.1:1"))
        .build();
    assert!(
        matches!(client, Err(HttpClientError::InvalidConfig(_))),
        "expected InvalidConfig, got {client:?}"
    );
}
