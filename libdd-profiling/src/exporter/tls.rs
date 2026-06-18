// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared TLS configuration for the profiling exporter.
//!
//! The exporter uses `ureq` for synchronous HTTPS requests and configures it to
//! use rustls with the platform verifier plus the workspace's preferred crypto
//! provider.

/// Wraps a [`ureq::tls::TlsConfig`] prepared for the profiling exporter.
#[derive(Clone)]
pub(crate) struct TlsConfig(pub(crate) ureq::tls::TlsConfig);

impl TlsConfig {
    pub fn new() -> Self {
        // Use an explicit CryptoProvider rather than relying on
        // `CryptoProvider::get_default_or_install_from_crate_features()`.
        // Feature unification may enable multiple crypto backends in the same
        // build, which causes the automatic detection to panic.
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(Self::default_crypto_provider()));

        let config = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(provider)
            .build();

        Self(config)
    }

    /// Returns the default crypto provider (ring for non-FIPS builds).
    fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
        rustls::crypto::ring::default_provider()
    }
}

static TLS_CONFIG: std::sync::LazyLock<Result<TlsConfig, String>> =
    std::sync::LazyLock::new(|| Ok(TlsConfig::new()));

pub(crate) fn cached_tls_config() -> anyhow::Result<TlsConfig> {
    TLS_CONFIG
        .as_ref()
        .map(Clone::clone)
        .map_err(|err| anyhow::anyhow!("{err}"))
}
