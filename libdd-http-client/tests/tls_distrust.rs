// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Live TLS regression tests: the reqwest backend must reject certificate
//! chains containing the compromised eMudhra public keys from
//! RUSTSEC-2026-0382.
//!
//! The affected endpoint serves a chain cross-signed by `AAA Certificate
//! Services`, a root present in common platform trust stores, so the chain
//! passes unmodified platform verification and only the application-level
//! distrust check rejects it. See https://emudhra.we-have-a-problem.com/.
//!
//! These tests require network access and are ignored under Miri. The
//! equivalent check for the hyper backend is covered by `libdd-common`'s
//! `tests/tls_distrust.rs`.

#![cfg(all(feature = "reqwest-backend", feature = "https"))]

use core::time::Duration;

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};

const AFFECTED_URL: &str = "https://dv-g3b-aaa-emudhra.we-have-a-problem.com/";
const CONTROL_URL: &str = "https://www.datadoghq.com/";
const DEADLINE: Duration = Duration::from_secs(30);

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reqwest_backend_rejects_compromised_ca_chain() {
    let client = HttpClient::new(AFFECTED_URL.to_owned(), DEADLINE).expect("valid client");
    let request = HttpRequest::new(HttpMethod::Get, AFFECTED_URL.to_owned());
    let error = tokio::time::timeout(DEADLINE, client.send(request))
        .await
        .expect("request completes")
        .expect_err("compromised chain must be rejected");
    let message = error.to_string();
    assert!(
        message.contains("ApplicationVerificationFailure"),
        "expected explicit certificate distrust, got: {message}"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn reqwest_backend_control_request_still_succeeds() {
    let client = HttpClient::new(CONTROL_URL.to_owned(), DEADLINE).expect("valid client");
    let request = HttpRequest::new(HttpMethod::Get, CONTROL_URL.to_owned());
    let response = tokio::time::timeout(DEADLINE, client.send(request))
        .await
        .expect("request completes")
        .expect("control handshake must succeed");
    assert!(
        response.status_code() < 400,
        "unexpected status code: {}",
        response.status_code()
    );
}
