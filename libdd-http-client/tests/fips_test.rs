// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "fips")]

use libdd_http_client::init_fips_crypto;

/// Verifies that init_fips_crypto() installs the aws-lc-rs provider and that a
/// second call returns an error. This confirms the `fips` feature wires up
/// aws-lc-rs rather than ring.
#[test]
fn fips_crypto_provider() {
    let first = init_fips_crypto();
    let second = init_fips_crypto();

    assert!(first.is_ok(), "init_fips_crypto() failed: {first:?}");
    assert!(second.is_err(), "expected Err on second call, got Ok");

    let err_msg = second.unwrap_err().to_string();
    assert!(
        err_msg.contains("already installed"),
        "unexpected error message: {err_msg}"
    );
}
