// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::future::BoxFuture;
use futures::{future, FutureExt};
use hyper_util::client::legacy::connect;

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::LazyLock;

#[cfg(unix)]
pub mod uds;

pub mod named_pipe;

pub mod errors;

mod conn_stream;
use conn_stream::{ConnStream, ConnStreamError};

#[derive(Clone)]
pub enum Connector {
    Http(connect::HttpConnector),
    #[cfg(feature = "tls-core")]
    Https(hyper_rustls::HttpsConnector<connect::HttpConnector>),
}

static DEFAULT_CONNECTOR: LazyLock<Connector> = LazyLock::new(Connector::new);

impl Default for Connector {
    fn default() -> Self {
        DEFAULT_CONNECTOR.clone()
    }
}

impl Connector {
    /// Make sure this function is not called frequently. Fetching the root certificates is an
    /// expensive operation. Access the globally cached connector via Connector::default().
    fn new() -> Self {
        #[cfg(feature = "tls-core")]
        {
            #[cfg(feature = "use_webpki_roots")]
            let https_connector_fn = https::build_https_connector_with_webpki_roots;
            #[cfg(not(feature = "use_webpki_roots"))]
            let https_connector_fn = https::build_https_connector;

            match https_connector_fn() {
                Ok(connector) => Connector::Https(connector),
                Err(_) => Connector::Http(connect::HttpConnector::new()),
            }
        }
        #[cfg(not(feature = "tls-core"))]
        {
            Connector::Http(connect::HttpConnector::new())
        }
    }

    fn build_conn_stream(
        &mut self,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> BoxFuture<'static, Result<ConnStream, ConnStreamError>> {
        match self {
            Self::Http(c) => {
                if require_tls {
                    future::err::<ConnStream, ConnStreamError>(
                        errors::Error::CannotEstablishTlsConnection.into(),
                    )
                    .boxed()
                } else {
                    ConnStream::from_http_connector_with_uri(c, uri).boxed()
                }
            }
            #[cfg(feature = "tls-core")]
            Self::Https(c) => {
                ConnStream::from_https_connector_with_uri(c, uri, require_tls).boxed()
            }
        }
    }
}

#[cfg(feature = "tls-core")]
mod https {
    #[cfg(feature = "use_webpki_roots")]
    use hyper_rustls::ConfigBuilderExt;

    use rustls::ClientConfig;

    /// Ensures the rustls default CryptoProvider is installed (ring for non-FIPS).
    /// In FIPS mode, the caller must install the FIPS provider before any TLS use.
    #[cfg(feature = "https")]
    fn ensure_crypto_provider_initialized() {
        use std::sync::Once;

        static INIT_CRYPTO_PROVIDER: Once = Once::new();

        INIT_CRYPTO_PROVIDER.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// In FIPS mode, the caller must install the FIPS-compliant crypto provider
    /// (e.g., aws-lc-rs FIPS) before any TLS connections are established.
    #[cfg(not(feature = "https"))]
    fn ensure_crypto_provider_initialized() {}

    #[cfg(feature = "use_webpki_roots")]
    pub(super) fn build_https_connector_with_webpki_roots() -> anyhow::Result<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    > {
        ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed

        let client_config = ClientConfig::builder()
            .with_webpki_roots()
            .with_no_client_auth();
        Ok(hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config)
            .https_or_http()
            .enable_http1()
            .build())
    }

    #[cfg(not(feature = "use_webpki_roots"))]
    /// Returns a default connector that uses the system trust roots.
    /// `SSL_CERT_FILE` and `SSL_CERT_DIR` variable are only supported on linux, see
    /// `rustls_platform_verifier` doc for details.
    pub(super) fn build_https_connector() -> anyhow::Result<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    > {
        use rustls_platform_verifier::BuilderVerifierExt;

        ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed

        let client_config = ClientConfig::builder()
            .with_platform_verifier()?
            .with_no_client_auth();

        Ok(hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config)
            .https_or_http()
            .enable_http1()
            .build())
    }
}

impl tower_service::Service<hyper::Uri> for Connector {
    type Response = ConnStream;
    type Error = ConnStreamError;

    // This lint gets lifted in this place in a newer version, see:
    // https://github.com/rust-lang/rust-clippy/pull/8030
    #[allow(clippy::type_complexity)]
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&mut self, uri: hyper::Uri) -> Self::Future {
        match uri.scheme_str() {
            Some("unix") => conn_stream::ConnStream::from_uds_uri(uri).boxed(),
            Some("windows") => conn_stream::ConnStream::from_named_pipe_uri(uri).boxed(),
            Some("https") => self.build_conn_stream(uri, true),
            _ => self.build_conn_stream(uri, false),
        }
    }

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self {
            Connector::Http(c) => c.poll_ready(cx).map_err(|e| e.into()),
            #[cfg(feature = "tls-core")]
            Connector::Https(c) => c.poll_ready(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::http_common;
    #[cfg(any(feature = "use_webpki_roots", target_os = "linux"))]
    use {super::*, std::env};
    #[cfg(feature = "tls-core")]
    use {crate::http_common::Body, hyper::Request};

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    /// Verify that the Connector type implements the correct bound Connect + Clone
    /// to be able to use the hyper::Client
    fn test_hyper_client_from_connector() {
        let _ = http_common::new_default_client();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
    fn test_hyper_client_from_connector_with_webpki_roots() {
        let _ = http_common::new_default_client();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    // Only Linux eagerly loads roots at connector construction; macOS/Windows verify lazily
    // during the TLS handshake, so SSL_CERT_FILE/SSL_CERT_DIR cannot be exercised there.
    #[cfg(target_os = "linux")]
    /// Verify that Connector falls back to Http when native root certificates
    /// are not available and webpki roots are not enabled.
    fn test_missing_root_certificates_only_allow_http_connections() {
        const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";
        const ENV_SSL_CERT_DIR: &str = "SSL_CERT_DIR";
        let old_value = env::var(ENV_SSL_CERT_FILE).unwrap_or_default();
        let old_dir_value = env::var(ENV_SSL_CERT_DIR).unwrap_or_default();

        env::set_var(ENV_SSL_CERT_FILE, "this/folder/does/not/exist");
        env::set_var(ENV_SSL_CERT_DIR, "this/folder/does/not/exist");
        let connector = Connector::new();

        assert!(matches!(connector, Connector::Http(_)));

        env::set_var(ENV_SSL_CERT_FILE, old_value);
        env::set_var(ENV_SSL_CERT_DIR, old_dir_value);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
    #[cfg(feature = "tls-core")]
    /// Verify that Connector builds an Https connector using webpki certificates
    /// even when native root certificates are not available.
    fn test_missing_root_certificates_use_webpki_certificates() {
        const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";
        let old_value = env::var(ENV_SSL_CERT_FILE).unwrap_or_default();

        env::set_var(ENV_SSL_CERT_FILE, "this/folder/does/not/exist");
        let connector = Connector::new();
        assert!(matches!(connector, Connector::Https(_)));

        env::set_var(ENV_SSL_CERT_FILE, old_value);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "tls-core")]
    /// Verify that a HTTPS GET request succeeds using
    /// the default Connector (native platform TLS verifier or webpki roots).
    async fn test_https_request_succeeds() {
        let client = http_common::new_default_client();
        let request = Request::get("https://www.datadoghq.com")
            .body(Body::empty())
            .expect("failed to build request");
        let response = client
            .request(request)
            .await
            .expect("HTTPS request to datadoghq.com failed");
        let status = response.status();
        // Accept any successful (2xx) or redirect (3xx) response.
        assert!(
            status.is_success() || status.is_redirection(),
            "unexpected status code: {status}"
        );
    }
}
