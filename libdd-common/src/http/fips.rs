// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FIPS crypto provider initialisation.
//!
//! When the `fips` feature is enabled the rustls default crypto provider is
//! intentionally left empty; callers must install the FIPS-compliant
//! aws-lc-rs provider exactly once per process before any TLS use.
//!
//! [`install_fips_provider`] is the canonical libdatadog entry point for this
//! installation. Higher layers (e.g., `libdd-http-client::init_fips_crypto`)
//! are expected to delegate here so the install runs once even when multiple
//! crates would otherwise each try.
//!
//! This module is conditionally included by its parent (`super::mod`) when the
//! `fips` feature is enabled, so no inner `#![cfg(...)]` attribute is needed
//! here.

use thiserror::Error;

/// Failures from [`install_fips_provider`].
#[derive(Debug, Error)]
pub enum FipsInstallError {
    /// A default rustls crypto provider has already been installed for this
    /// process. Subsequent calls are rejected because rustls supports a single
    /// process-wide default provider.
    #[error("a rustls default crypto provider is already installed")]
    AlreadyInstalled,
}

/// Install the FIPS-compliant aws-lc-rs rustls provider as the process-wide
/// default. Must be called at most once per process, before any TLS use.
///
/// Returns [`FipsInstallError::AlreadyInstalled`] if a default provider has
/// already been installed.
pub fn install_fips_provider() -> Result<(), FipsInstallError> {
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .map_err(|_| FipsInstallError::AlreadyInstalled)
}
