// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Rejects compromised public keys in presented server certificates.
//!
//! The upstream WebPKI fix for these compromised keys (RUSTSEC-2026-0382, fixed
//! in `rustls-webpki` 0.103.16) requires an `aws-lc-rs` version outside the
//! current FIPS dependency constraints. This wrapper applies the equivalent
//! application-level mitigation without changing any dependency versions.
//! Remove this wrapper when the upstream fix is compatible with the approved
//! crypto dependencies.
//!
//! This check is deliberately conservative: a blocked certificate anywhere in
//! the presented chain rejects the connection, even if path building could
//! ignore it or find an alternative valid path. Every presented certificate
//! must also parse successfully, including unused certificates.
//!
//! Unlike the upstream fix, the wrapper cannot filter trust anchors held by
//! the platform verifier, since the platform trust store is opaque. The
//! compromised keys belong to intermediate CAs that must be presented by the
//! server to be used, so checking the presented chain closes the practical
//! attack paths.

use alloc::sync::Arc;
use std::{collections::HashMap, sync::LazyLock};

use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
};

// Compare public keys to also reject reissued and cross-signed certificates using the same keys.
// These fixed, complete DER SPKIs have a two-byte SEQUENCE header followed by 118 bytes of
// contents. See data/distrusted/README.md for provenance and encoding.
const DISTRUSTED_SPKI_BYTES: &[&[u8; 120]] = &[
    include_bytes!("data/distrusted/EMDVTLSCAG3B.spki.der"),
    include_bytes!("data/distrusted/EMDVTLSCAG3C.spki.der"),
    include_bytes!("data/distrusted/EMEVTLSCAG3B.spki.der"),
    include_bytes!("data/distrusted/EMEVTLSCAG3C.spki.der"),
    include_bytes!("data/distrusted/EMOVTLSCAG3B.spki.der"),
    include_bytes!("data/distrusted/EMOVTLSCAG3C.spki.der"),
    include_bytes!("data/distrusted/EMOVCSCAG3B.spki.der"),
    include_bytes!("data/distrusted/EMOVCSCAG3C.spki.der"),
];

// Indexed blocklist so every presented certificate is checked with a single lookup instead
// of scanning the list. `LazyLock` is required because a `HashMap` cannot be built in `const`
// context; the blocklist is fixed at compile time and never grows afterwards.
static DISTRUSTED_SPKIS: LazyLock<HashMap<&'static [u8], ()>> = LazyLock::new(|| {
    DISTRUSTED_SPKI_BYTES
        .iter()
        .map(|spki| (spki.as_slice(), ()))
        .collect()
});

/// A [`ServerCertVerifier`] that rejects any certificate chain containing a
/// known-compromised public key before delegating to the wrapped verifier.
///
/// Hostname, expiry, certificate signatures, and handshake signatures remain
/// the responsibility of the wrapped verifier.
#[derive(Debug)]
pub(super) struct DistrustServerCertVerifier {
    inner: Arc<dyn ServerCertVerifier>,
}

impl DistrustServerCertVerifier {
    /// Wraps `inner` with the compromised-key distrust check.
    pub(super) fn new(inner: Arc<dyn ServerCertVerifier>) -> Self {
        Self { inner }
    }

    /// Builds the platform verifier wrapped with the compromised-key distrust check.
    ///
    /// # Errors
    ///
    /// If the platform verifier cannot be initialized, an error will be returned.
    pub(super) fn platform_with_distrust(provider: Arc<CryptoProvider>) -> Result<Self, Error> {
        Ok(Self::new(Arc::new(
            rustls_platform_verifier::Verifier::new(provider)?,
        )))
    }
}

impl ServerCertVerifier for DistrustServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        for cert in core::iter::once(end_entity).chain(intermediates) {
            let spki = ParsedCertificate::try_from(cert)?.subject_public_key_info();
            if DISTRUSTED_SPKIS.contains_key(spki.as_ref()) {
                return Err(Error::InvalidCertificate(
                    CertificateError::ApplicationVerificationFailure,
                ));
            }
        }

        self.inner
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }

    fn requires_raw_public_keys(&self) -> bool {
        self.inner.requires_raw_public_keys()
    }

    fn root_hint_subjects(&self) -> Option<&[DistinguishedName]> {
        self.inner.root_hint_subjects()
    }
}
