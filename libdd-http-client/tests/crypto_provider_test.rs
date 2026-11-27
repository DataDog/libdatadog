// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Each test runs in its own process under nextest, so no provider is installed beforehand.

#![cfg(all(feature = "reqwest-backend", feature = "https"))]

use libdd_http_client::HttpClient;
use std::time::Duration;

#[test]
fn a_plain_http_client_builds_without_a_preinstalled_crypto_provider() {
    let client = HttpClient::new("http://localhost:1".to_owned(), Duration::from_secs(1));
    assert!(client.is_ok(), "{:?}", client.err());
}

#[test]
fn an_https_client_builds_without_a_preinstalled_crypto_provider() {
    let client = HttpClient::new("https://localhost:1".to_owned(), Duration::from_secs(1));
    assert!(client.is_ok(), "{:?}", client.err());
}
