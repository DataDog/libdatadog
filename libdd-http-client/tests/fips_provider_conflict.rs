// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! When something else in the process installs a non-FIPS provider first, a FIPS
//! build must report that conflict instead of quietly exporting over ring. The
//! provider slot is process-wide and fills only once, so losing the race is a
//! real outcome — `https` links ring alongside the FIPS provider, and any crate
//! in the host process can install it.
//!
//! Requires both features: `fips` for the client behaviour under test, `https`
//! for ring to be linked at all so the test can install it. Lives in its own
//! test binary because installing a provider is a one-way, process-wide effect.
#![cfg(all(feature = "fips", feature = "https"))]

use libdd_http_client::{HttpClient, HttpClientError};
use std::time::Duration;

#[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
#[test]
fn non_fips_provider_installed_first_is_reported() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("nothing else may have installed a provider in this process");

    let err = HttpClient::new("https://localhost:8126".to_owned(), Duration::from_secs(3))
        .expect_err("a FIPS build must not accept ring");

    assert!(
        matches!(err, HttpClientError::InvalidConfig(ref msg) if msg.contains("non-FIPS")),
        "unexpected error: {err:?}"
    );
}
