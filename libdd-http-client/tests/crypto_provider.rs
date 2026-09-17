// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A client must be constructible without the caller installing a rustls crypto
//! provider first.
//!
//! reqwest is built with `rustls-no-provider`, so it takes the process-wide
//! default provider and panics while building its client when there is none.
//! Nothing in this test may install a provider: that is exactly what is being
//! verified, and an install here would hide the regression.
#![cfg(all(feature = "https", not(feature = "fips")))]

use libdd_http_client::HttpClient;
use std::time::Duration;

#[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
#[test]
fn client_builds_without_caller_installed_crypto_provider() {
    let client = HttpClient::new("https://localhost:8126".to_owned(), Duration::from_secs(3));

    assert!(
        client.is_ok(),
        "client construction failed: {:?}",
        client.err()
    );
}
