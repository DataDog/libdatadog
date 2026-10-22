// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Live TLS regression tests: both HTTP clients must reject certificate chains
//! containing the compromised eMudhra public keys from RUSTSEC-2026-0382.
//!
//! The affected endpoint serves a chain cross-signed by `AAA Certificate
//! Services`, a root present in common platform trust stores, so the chain
//! passes unmodified platform verification and only the application-level
//! distrust check rejects it. See https://emudhra.we-have-a-problem.com/.

#![cfg(all(
    feature = "https",
    feature = "reqwest",
    not(feature = "use_webpki_roots")
))]

use core::time::Duration;

use hyper::Request;
use libdd_common::Endpoint;
use libdd_common::http_common::{self, Body};

const AFFECTED_URL: &str = "https://dv-g3b-aaa-emudhra.we-have-a-problem.com/";
const CONTROL_URL: &str = "https://www.datadoghq.com/";
const DEADLINE: Duration = Duration::from_secs(30);

/// With `rustls-no-provider`, no crypto provider is auto-installed.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Renders an error together with its whole source chain, since the TLS
/// rejection reason only appears in nested sources.
fn error_chain(error: &dyn core::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }
    chain
}

fn assert_distrust(error: &dyn core::error::Error) {
    let chain = error_chain(error);
    assert!(
        chain.contains("ApplicationVerificationFailure"),
        "expected explicit certificate distrust, got: {chain}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn hyper_client_rejects_compromised_ca_chain() {
    ensure_crypto_provider();
    let client = http_common::new_default_client();
    let request = Request::get(AFFECTED_URL)
        .body(Body::empty())
        .expect("valid request");
    let error = tokio::time::timeout(DEADLINE, client.request(request))
        .await
        .expect("request completes")
        .expect_err("compromised chain must be rejected");
    assert_distrust(&error);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reqwest_client_rejects_compromised_ca_chain() {
    ensure_crypto_provider();
    let endpoint = Endpoint::from_slice(AFFECTED_URL);
    let (builder, _) = endpoint
        .to_reqwest_client_builder()
        .expect("valid client builder");
    let client = builder.build().expect("valid client");
    let error = tokio::time::timeout(DEADLINE, client.get(AFFECTED_URL).send())
        .await
        .expect("request completes")
        .expect_err("compromised chain must be rejected");
    assert_distrust(&error);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn hyper_client_control_request_still_succeeds() {
    ensure_crypto_provider();
    let client = http_common::new_default_client();
    let request = Request::get(CONTROL_URL)
        .body(Body::empty())
        .expect("valid request");
    let response = tokio::time::timeout(DEADLINE, client.request(request))
        .await
        .expect("request completes")
        .expect("control handshake must succeed");
    let status = response.status();
    assert!(
        status.is_success() || status.is_redirection(),
        "unexpected status code: {status}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reqwest_client_control_request_still_succeeds() {
    ensure_crypto_provider();
    let endpoint = Endpoint::from_slice(CONTROL_URL);
    let (builder, _) = endpoint
        .to_reqwest_client_builder()
        .expect("valid client builder");
    let client = builder.build().expect("valid client");
    let response = tokio::time::timeout(DEADLINE, client.get(CONTROL_URL).send())
        .await
        .expect("request completes")
        .expect("control handshake must succeed");
    let status = response.status();
    assert!(
        status.is_success() || status.is_redirection(),
        "unexpected status code: {status}"
    );
}
