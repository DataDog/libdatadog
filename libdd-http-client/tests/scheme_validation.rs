// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Cross-scheme redirect regression test: plain-HTTP clients must not
//! follow redirects to URLs whose scheme the client was not configured
//! with.
//!
//! A plain-HTTP client has no preconfigured TLS backend (building one would
//! eagerly load the system CA roots, which must not happen in minimal
//! environments), so following an http-to-https redirect would negotiate TLS
//! on reqwest's default verifier, bypassing the compromised-key distrust
//! check (see `src/tls.rs`). Such redirects are therefore stopped and the 3xx
//! response is returned to the caller. Scheme validation of request URLs
//! themselves is covered by unit tests in `src/backend/reqwest_backend.rs`.

#![cfg(all(feature = "reqwest-backend", any(feature = "https", feature = "fips")))]

use httpmock::prelude::*;
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use std::time::Duration;

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg_attr(miri, ignore)] // real reqwest client construction is prohibitively slow under Miri
#[tokio::test]
async fn plain_http_client_does_not_follow_cross_scheme_redirects() {
    ensure_crypto_provider();
    let server = MockServer::start_async().await;

    let mock = server
        .mock_async(|when, then| {
            when.method(GET).path("/moved");
            // Redirect to a scheme the client was not configured for; it must
            // not be followed, since the plain-HTTP client has no preconfigured
            // (distrust-checked) TLS backend.
            then.status(301)
                .header("Location", "https://127.0.0.1:9/redirected");
        })
        .await;

    let client = HttpClient::new(server.url("/"), Duration::from_secs(5)).unwrap();
    let request = HttpRequest::new(HttpMethod::Get, server.url("/moved"));
    let response = client
        .send(request)
        .await
        .expect("redirect response is returned");

    // The redirect is stopped: the 3xx response itself is returned to the
    // caller and no TLS connection to the redirected URL is ever attempted.
    assert_eq!(response.status_code(), 301);
    mock.assert_async().await;
}
