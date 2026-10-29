// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pre-initialized TLS configuration for high-performance profile export.
//!
//! On Linux, [`TlsConfig::new`] eagerly loads native root certificates via the
//! platform verifier, avoiding repeated expensive disk I/O on every
//! [`ProfileExporter`] creation.
//!
//! On macOS, the platform verifier's `Verifier::new()` is cheap (no cert
//! loading), but the actual Security.framework work happens lazily during each
//! TLS handshake. Creating a [`TlsConfig`] still avoids redundant `reqwest`
//! client setup on every exporter creation.
//!
//! # Fork Safety
//!
//! `TlsConfig` does **not** call Security.framework APIs directly, so it is
//! safe to create before `fork()`. Security.framework work is deferred to
//! each child's first TLS handshake.
//!
//! [`ProfileExporter`]: super::ProfileExporter

/// Wraps a [`rustls::ClientConfig`] that has been pre-configured with the
/// platform certificate verifier. Clone is cheap (inner `Arc`).
#[derive(Clone)]
pub(crate) struct TlsConfig(pub(crate) rustls::ClientConfig);

impl TlsConfig {
    /// Create a new TLS configuration using the platform certificate verifier.
    ///
    /// On Linux, this eagerly loads the native root certificate store, which is
    /// the expensive operation that was previously repeated on every
    /// `ProfileExporter::new` call.
    ///
    /// On macOS, this is lightweight; the platform verifier defers
    /// Security.framework calls to the first TLS handshake.
    pub fn new() -> Result<Self, rustls::Error> {
        use rustls_platform_verifier::BuilderVerifierExt;

        // Use an explicit CryptoProvider rather than relying on
        // `CryptoProvider::get_default_or_install_from_crate_features()`.
        // Feature unification may enable multiple crypto backends in the same
        // build, which causes the automatic detection to panic.
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(Self::default_crypto_provider()));

        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth();
        Ok(Self(config))
    }

    /// Returns the default crypto provider (ring for non-FIPS builds).
    ///
    /// Matches the convention used by `libdd-common`: ring on all platforms
    /// for non-FIPS. FIPS builds install the aws-lc-rs FIPS provider externally.
    fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
        rustls::crypto::ring::default_provider()
    }
}

impl TlsConfig {
    /// Create a minimal TLS configuration with an empty root store.
    ///
    /// Used for non-HTTPS endpoints (HTTP, unix sockets, named pipes) where TLS
    /// will never actually be negotiated. Providing this to reqwest prevents it
    /// from attempting to load system CA certificates on its own, which fails in
    /// minimal container environments that have no CA certificates installed.
    pub fn new_empty() -> Result<Self, rustls::Error> {
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(Self::default_crypto_provider()));

        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        Ok(Self(config))
    }
}

static TLS_CONFIG: std::sync::LazyLock<Result<TlsConfig, String>> =
    std::sync::LazyLock::new(|| {
        TlsConfig::new().map_err(|err| format!("failed to initialize TLS configuration: {err}"))
    });

pub(crate) fn cached_tls_config() -> anyhow::Result<TlsConfig> {
    TLS_CONFIG
        .as_ref()
        .map(Clone::clone)
        .map_err(|err| anyhow::anyhow!("{err}"))
}

/// Returns a TLS config with an empty root store, for use with non-HTTPS endpoints.
/// Never fails — no system cert loading is attempted.
pub(crate) fn empty_tls_config() -> anyhow::Result<TlsConfig> {
    TlsConfig::new_empty().map_err(|err| anyhow::anyhow!("failed to build TLS config: {err}"))
}
