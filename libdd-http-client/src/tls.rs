// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! TLS configuration for the reqwest backend.
//!
//! The reqwest backend routes HTTPS requests through the TLS configuration
//! built here instead of reqwest's own platform verifier, so that the
//! compromised-key distrust check (see [`certificate_distrust`]) and the
//! selected crypto provider apply. Without this, reqwest would build its own
//! platform verifier, bypassing the distrust wrapper.
//!
//! This mirrors `libdd-common::connector::https::build_tls_config` (used by
//! the hyper backend through `libdd-common`'s connector) for builds that do
//! not depend on `libdd-common`. Keep both in sync.

mod certificate_distrust;

// Skipped under Miri: the certificate distrust tests exercise ring-backed
// signature verification, whose FFI cannot execute there.
#[cfg(all(test, not(miri)))]
mod certificate_distrust_tests;

use std::sync::Arc;

use crate::HttpClientError;

/// Ensures the rustls default CryptoProvider is installed (ring for non-FIPS).
/// In FIPS mode, the caller must install the FIPS provider before any TLS use.
#[cfg(feature = "https")]
fn ensure_crypto_provider_initialized() {
    use std::sync::Once;

    static INIT_CRYPTO_PROVIDER: Once = Once::new();

    INIT_CRYPTO_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// In FIPS mode, the caller must install the FIPS-compliant crypto provider
/// (e.g. via [`crate::init_fips_crypto`]) before any TLS connections are
/// established.
#[cfg(not(feature = "https"))]
fn ensure_crypto_provider_initialized() {}

/// Builds the client TLS config for the reqwest backend using the system
/// trust roots. `SSL_CERT_FILE` and `SSL_CERT_DIR` variables are only
/// supported on linux, see `rustls_platform_verifier` doc for details.
///
/// The platform verifier is wrapped with an application-level distrust check that
/// rejects known-compromised public keys, until `rustls-webpki` can be updated to
/// a version carrying the upstream fix (see [`certificate_distrust`]).
pub(crate) fn build_tls_config() -> Result<rustls::ClientConfig, HttpClientError> {
    ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed

    let builder = rustls::ClientConfig::builder();
    let verifier = certificate_distrust::DistrustServerCertVerifier::platform_with_distrust(
        builder.crypto_provider().clone(),
    )
    .map_err(|e| HttpClientError::InvalidConfig(e.to_string()))?;
    Ok(builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}
