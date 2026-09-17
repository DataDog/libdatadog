// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crypto provider selection for the rustls-backed TLS stack.
//!
//! The `https` and `fips` features each link a rustls crypto provider (ring and
//! FIPS-mode aws-lc-rs respectively) and are meant to be alternatives. Cargo
//! features are additive, so feature unification can enable both in one build;
//! the cfgs below resolve that at compile time by letting `fips` win, so the
//! selection never depends on the order in which a caller initializes things.
//!
//! Every configuration must end up with a provider installed, because reqwest is
//! built with `rustls-no-provider`: it takes the process-wide default and panics
//! while building its client when there is none. A panic there would cross an FFI
//! boundary and abort the host process.
//!
//! The provider slot is process-wide and can only be filled once, so a FIPS build
//! cannot guarantee it wins the race — it can only check what it ended up with
//! and refuse to run on a non-FIPS provider.

use crate::HttpClientError;

/// Ensures a rustls crypto provider is installed before a TLS-capable client is
/// constructed.
///
/// Non-FIPS builds install ring. A provider the caller installed first is left in
/// place, so an explicit choice still wins.
#[cfg(all(feature = "https", not(feature = "fips")))]
pub(crate) fn ensure_crypto_provider() -> Result<(), HttpClientError> {
    use std::sync::Once;

    static INIT_CRYPTO_PROVIDER: Once = Once::new();

    INIT_CRYPTO_PROVIDER.call_once(|| {
        // Fails only if a provider is already installed, which is the outcome we
        // want to keep: this build has no requirement the installed one can fail.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });

    Ok(())
}

/// FIPS builds install aws-lc-rs, which the `fips` feature builds in FIPS mode
/// (via `rustls/fips`), so this is the same provider [`crate::init_fips_crypto`]
/// installs; a caller that calls that first still wins.
///
/// The install is best-effort because the process-wide slot can only be filled
/// once, and losing that race is not hypothetical: `https` may also be enabled,
/// which links ring, and any other crate in the process can install it. So the
/// outcome is verified rather than assumed — a non-FIPS provider in a FIPS build
/// is reported to the caller instead of being used silently.
#[cfg(feature = "fips")]
pub(crate) fn ensure_crypto_provider() -> Result<(), HttpClientError> {
    use std::sync::Once;

    static INIT_CRYPTO_PROVIDER: Once = Once::new();

    INIT_CRYPTO_PROVIDER.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    match rustls::crypto::CryptoProvider::get_default() {
        Some(provider) if provider.fips() => Ok(()),
        Some(_) => Err(HttpClientError::InvalidConfig(
            "a non-FIPS rustls CryptoProvider is already installed process-wide, so this FIPS \
             build cannot use it; install the FIPS provider with init_fips_crypto() before \
             anything else installs one"
                .to_owned(),
        )),
        // Unreachable in practice: the install above either succeeded or lost to
        // another provider. Reported rather than unwrapped all the same.
        None => Err(HttpClientError::InvalidConfig(
            "no rustls CryptoProvider could be installed for this FIPS build".to_owned(),
        )),
    }
}

/// Without `https` or `fips` no TLS stack is compiled into the client, so there
/// is no provider to select.
#[cfg(not(any(feature = "https", feature = "fips")))]
pub(crate) fn ensure_crypto_provider() -> Result<(), HttpClientError> {
    Ok(())
}
