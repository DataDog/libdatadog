// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! TLS distrust regression tests: both HTTP clients must reject certificate
//! chains containing the compromised eMudhra public keys from
//! RUSTSEC-2026-0382.
//!
//! The clients connect to a local TLS server instead of public endpoints, so
//! the tests never leave the loopback interface. The server presents a
//! self-signed `localhost` certificate trusted through `SSL_CERT_FILE`, so
//! control handshakes pass the unmodified platform verifier. In the affected
//! case, the server additionally presents a captured eMudhra intermediate with
//! a compromised public key: the chain is otherwise valid, and only the
//! application-level distrust check rejects it. This mirrors the integration
//! test from DataDog/saluki (see `lib/saluki-io/tests/certificate_distrust.rs`).
//!
//! `SSL_CERT_FILE` is only honored by the platform verifier on non-Apple Unix
//! targets (see `rustls_platform_verifier`), hence the target gating. Other
//! platforms still exercise the verifier itself through the deterministic
//! captured-chain unit tests in `src/connector/certificate_distrust_tests.rs`.
//!
//! This integration test sets environment variables, so its binary contains a
//! single test, run before any HTTP client is built.

#![cfg(all(
    unix,
    not(target_os = "android"),
    not(target_vendor = "apple"),
    feature = "https",
    feature = "reqwest",
    not(feature = "use_webpki_roots")
))]

// The crate lints require `alloc`/`core` imports over their `std` aliases; `alloc`
// needs an explicit `extern crate` in integration tests.
extern crate alloc;

use alloc::sync::Arc;
use core::time::Duration;
use std::{env, fs, path::Path};

use hyper::Request;
use libdd_common::Endpoint;
use libdd_common::http_common::{self, Body};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, pem::PemObject as _},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};
use tokio_rustls::TlsAcceptor;

/// Intermediate from a captured affected chain, whose public key is compromised
/// (RUSTSEC-2026-0382). See `data/distrusted/README.md`.
const AFFECTED_CHAIN: &[u8] = include_bytes!("data/distrusted/dv-g3b-emudhra.pem");
const DEADLINE: Duration = Duration::from_secs(5);

/// A self-signed certificate and its private key, generated with `rcgen`.
struct SelfSignedCert {
    cert_der: CertificateDer<'static>,
    key_der: Vec<u8>,
    cert_pem: String,
}

impl SelfSignedCert {
    fn localhost() -> Self {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
                .expect("rcgen should generate a self-signed certificate");
        Self {
            cert_der: cert.der().clone(),
            key_der: key_pair.serialize_der(),
            cert_pem: cert.pem(),
        }
    }

    /// Returns the certificate chain (the single self-signed certificate).
    fn cert_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![self.cert_der.clone()]
    }

    fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }

    fn write_cert_pem(&self, path: &Path) {
        fs::write(path, &self.cert_pem).expect("should write self-signed certificate to path");
    }
}

fn server_config(cert: &SelfSignedCert, blocked_extra: bool) -> Arc<ServerConfig> {
    let mut certificates = cert.cert_chain();
    if blocked_extra {
        // A blocked extra certificate makes otherwise valid handshakes fail
        // without needing a compromised key.
        let intermediate = CertificateDer::pem_slice_iter(AFFECTED_CHAIN)
            .nth(1)
            .unwrap()
            .unwrap();
        certificates.push(intermediate);
    }
    Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, cert.private_key())
            .unwrap(),
    )
}

async fn read_headers(stream: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(stream.read_u8().await.expect("complete HTTP headers"));
        assert!(headers.len() < 8192);
    }
    headers
}

/// Spawns a one-shot local TLS server: it accepts a single connection, reads
/// the request headers, and answers with an empty `200 OK` response.
async fn serve_origin(config: Arc<ServerConfig>) -> (http::Uri, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        if let Ok(mut tls) = TlsAcceptor::from(config).accept(stream).await {
            read_headers(&mut tls).await;
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        }
    });
    (format!("https://localhost:{port}/").parse().unwrap(), task)
}

/// With `rustls-no-provider`, no crypto provider is auto-installed.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Renders an error together with its whole source chain, since the TLS
/// rejection reason only appears in nested sources.
fn error_chain(error: &dyn core::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }
    chain
}

fn assert_distrust(error: &dyn core::error::Error) {
    let chain = error_chain(error);
    assert!(
        chain.contains("ApplicationVerificationFailure"),
        "expected explicit certificate distrust, got: {chain}"
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn all_http_clients_enforce_distrust() {
    // This integration-test process contains one test. Configure the platform
    // roots through `SSL_CERT_FILE` before any client or async task is created.
    ensure_crypto_provider();
    let cert = SelfSignedCert::localhost();
    let directory = tempfile::tempdir().unwrap();
    let root_path = directory.path().join("roots.pem");
    cert.write_cert_pem(&root_path);
    unsafe {
        env::set_var("SSL_CERT_FILE", &root_path);
        env::remove_var("SSL_CERT_DIR");
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for blocked_extra in [false, true] {
                let config = server_config(&cert, blocked_extra);

                // Hyper client through libdd-common's default connector.
                let (origin, origin_task) = serve_origin(config.clone()).await;
                let client = http_common::new_default_client();
                let request = Request::get(origin.clone())
                    .body(Body::empty())
                    .expect("valid request");
                let result = timeout(DEADLINE, client.request(request))
                    .await
                    .expect("request completes");
                if blocked_extra {
                    let error = result.expect_err("blocked certificate must fail TLS");
                    assert_distrust(&error);
                } else {
                    let response = result.expect("control handshake succeeds");
                    assert_eq!(response.status(), 200);
                }
                origin_task.abort();

                // Reqwest client through the endpoint's reqwest builder.
                let (origin, origin_task) = serve_origin(config.clone()).await;
                let endpoint = Endpoint::from_slice(&origin.to_string());
                let (builder, _) = endpoint
                    .to_reqwest_client_builder()
                    .expect("valid client builder");
                let client = builder.build().expect("valid client");
                let result = timeout(DEADLINE, client.get(origin.to_string()).send())
                    .await
                    .expect("request completes");
                if blocked_extra {
                    let error = result.expect_err("blocked certificate must fail TLS");
                    assert_distrust(&error);
                } else {
                    let response = result.expect("control handshake succeeds");
                    assert!(
                        response.status().is_success(),
                        "unexpected status code: {}",
                        response.status()
                    );
                }
                origin_task.abort();
            }
        });
}
