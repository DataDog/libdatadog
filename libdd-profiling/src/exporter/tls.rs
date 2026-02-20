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
pub struct TlsConfig(pub(crate) rustls::ClientConfig);

impl TlsConfig {
    /// Create a new TLS configuration using the platform certificate verifier.
    ///
    /// On Linux, this eagerly loads the native root certificate store, which is
    /// the expensive operation that was previously repeated on every
    /// `ProfileExporter::new` call.
    ///
    /// On macOS, this is lightweight — the platform verifier defers
    /// Security.framework calls to the first TLS handshake.
    pub fn new() -> Result<Self, rustls::Error> {
        use rustls_platform_verifier::ConfigVerifierExt;
        let config = rustls::ClientConfig::with_platform_verifier()?;
        Ok(Self(config))
    }
}
