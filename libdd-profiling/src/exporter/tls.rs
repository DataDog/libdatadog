// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared TLS configuration for the profiling exporter.
//!
//! The exporter uses `ureq` for synchronous HTTPS requests and configures it to
//! use rustls with the platform verifier plus the workspace's preferred crypto
//! provider (`aws-lc-rs` on Unix, `ring` on non-Unix).

/// Wraps a [`ureq::tls::TlsConfig`] prepared for the profiling exporter.
#[derive(Clone)]
pub(crate) struct TlsConfig(pub(crate) ureq::tls::TlsConfig);

impl TlsConfig {
    pub fn new() -> Self {
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

    /// Returns the platform-appropriate default crypto provider.
    ///
    /// Matches the convention used by `libdd-common`: `aws-lc-rs` on Unix,
    /// `ring` on Windows (where `aws-lc-rs` has issues).
    fn default_crypto_provider() -> rustls::crypto::CryptoProvider {
        #[cfg(unix)]
        {
            rustls::crypto::aws_lc_rs::default_provider()
        }
        #[cfg(not(unix))]
        {
            rustls::crypto::ring::default_provider()
        }
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
