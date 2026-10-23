// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! TLS distrust regression tests: the reqwest backend must reject certificate
//! chains containing the compromised eMudhra public keys from
//! RUSTSEC-2026-0382.
//!
//! The backend connects to a local TLS server instead of a public endpoint, so
//! the tests never leave the loopback interface. The server presents a
//! self-signed `localhost` certificate trusted through `SSL_CERT_FILE`, so the
//! control handshake passes the unmodified platform verifier. In the affected
//! case, the server additionally presents a captured eMudhra intermediate with
//! a compromised public key: the chain is otherwise valid, and only the
//! application-level distrust check rejects it. This mirrors the integration
//! test from DataDog/saluki (see `lib/saluki-io/tests/certificate_distrust.rs`).
//!
//! `SSL_CERT_FILE` is only honored by the platform verifier on non-Apple Unix
//! targets (see `rustls_platform_verifier`), hence the target gating. Other
//! platforms still exercise the verifier itself through the deterministic
//! captured-chain unit tests in `src/tls/certificate_distrust_tests.rs`.
//!
//! This integration test sets environment variables, so its binary contains a
//! single test, run before any HTTP client is built. The equivalent check for
//! the hyper backend is covered by `libdd-common`'s `tests/tls_distrust.rs`.

#![cfg(all(
    unix,
    not(target_os = "android"),
    not(target_vendor = "apple"),
    feature = "reqwest-backend",
    feature = "https"
))]

use std::{env, fs, path::Path, sync::Arc, time::Duration};

use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
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
async fn serve_origin(config: Arc<ServerConfig>) -> (String, JoinHandle<()>) {
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
    (format!("https://localhost:{port}/"), task)
}

#[test]
#[cfg_attr(miri, ignore)]
fn reqwest_backend_enforces_distrust() {
    // This integration-test process contains one test. Configure the platform
    // roots through `SSL_CERT_FILE` before any client or async task is created.
    let _ = rustls::crypto::ring::default_provider().install_default();
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
                let (origin, origin_task) = serve_origin(server_config(&cert, blocked_extra)).await;
                let client = HttpClient::new(origin.clone(), DEADLINE).expect("valid client");
                let request = HttpRequest::new(HttpMethod::Get, origin);
                let result = timeout(DEADLINE, client.send(request))
                    .await
                    .expect("request completes");
                if blocked_extra {
                    let error = result.expect_err("blocked certificate must fail TLS");
                    let message = error.to_string();
                    assert!(
                        message.contains("ApplicationVerificationFailure"),
                        "expected explicit certificate distrust, got: {message}"
                    );
                } else {
                    let response = result.expect("control handshake succeeds");
                    assert!(
                        response.status_code() < 400,
                        "unexpected status code: {}",
                        response.status_code()
                    );
                }
                origin_task.abort();
            }
        });
}
