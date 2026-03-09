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
        // Feature unification can enable both `aws-lc-rs` and `ring` in the
        // same build (reqwest enables aws-lc-rs while libdd-common enables
        // ring on Windows), which causes the automatic detection to panic.
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| std::sync::Arc::new(Self::default_crypto_provider()));

        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth();
        Ok(Self(config))
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
    std::sync::LazyLock::new(|| {
        TlsConfig::new().map_err(|err| format!("failed to initialize TLS configuration: {err}"))
    });

pub(crate) fn cached_tls_config() -> anyhow::Result<TlsConfig> {
    TLS_CONFIG
        .as_ref()
        .map(Clone::clone)
        .map_err(|err| anyhow::anyhow!("{err}"))
}
