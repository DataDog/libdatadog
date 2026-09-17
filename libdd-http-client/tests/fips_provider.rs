// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A FIPS build must also be able to construct a client without the caller
//! installing a provider first, and the provider it ends up with must be the
//! FIPS one rather than ring — `https` and `fips` can both be active in one
//! build (any `--all-features` run), and only then are both linked.
//!
//! This lives in its own test binary because the provider is process-wide:
//! sharing a process with a test that installs one would make it vacuous.
#![cfg(feature = "fips")]

use libdd_http_client::{init_fips_crypto, HttpClient};
use std::time::Duration;

#[cfg_attr(miri, ignore)] // real TLS/HTTP client construction is prohibitively slow under Miri
#[test]
fn fips_build_installs_fips_provider_without_caller_action() {
    let client = HttpClient::new("https://localhost:8126".to_owned(), Duration::from_secs(3));
    assert!(
        client.is_ok(),
        "client construction failed: {:?}",
        client.err()
    );

    // Building the client fixed the process-wide provider, so an explicit install
    // now reports that one is already in place. That it is *this* error, and not a
    // successful install, is what shows the client did not leave the slot empty.
    let err = init_fips_crypto().expect_err("expected the provider to be installed already");
    assert!(
        err.to_string().contains("already installed"),
        "unexpected error: {err}"
    );

    // The installed provider must be FIPS-approved, not ring. This is what catches
    // a `fips` feature that links aws-lc-rs in its non-FIPS mode.
    let provider = rustls::crypto::CryptoProvider::get_default()
        .expect("a provider must be installed at this point");
    assert!(
        provider.fips(),
        "installed provider is not FIPS-approved; `fips` must build aws-lc-rs in FIPS mode"
    );
}
