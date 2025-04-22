// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::future::BoxFuture;
use futures::{future, FutureExt};
use hyper_util::client::legacy::connect;

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

#[cfg(unix)]
pub mod uds;

pub mod named_pipe;

pub mod errors;

mod conn_stream;
use conn_stream::{ConnStream, ConnStreamError};

#[derive(Clone)]
pub enum Connector {
    Http(connect::HttpConnector),
    #[cfg(feature = "https")]
    Https(hyper_rustls::HttpsConnector<connect::HttpConnector>),
}

// TODO: Move to the more ergonomic LazyLock when MSRV is 1.80
static DEFAULT_CONNECTOR: OnceLock<Connector> = OnceLock::new();
fn get_default_connector() -> &'static Connector {
    DEFAULT_CONNECTOR.get_or_init(Connector::new)
}

impl Default for Connector {
    fn default() -> Self {
        get_default_connector().clone()
    }
}

impl Connector {
    /// Make sure this function is not called frequently. Fetching the root certificates is an
    /// expensive operation. Access the globally cached connector via Connector::default().
    fn new() -> Self {
        #[cfg(feature = "https")]
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
        #[cfg(not(feature = "https"))]
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
            #[cfg(feature = "https")]
            Self::Https(c) => {
                ConnStream::from_https_connector_with_uri(c, uri, require_tls).boxed()
            }
        }
    }
}

#[cfg(feature = "https")]
mod https {
    #[cfg(feature = "use_webpki_roots")]
    use hyper_rustls::ConfigBuilderExt;

    use rustls::ClientConfig;

    /// When using aws-lc-rs, rustls needs to be initialized with the default CryptoProvider;
    /// sometimes this is done as a side-effect of other operations, but we need to ensure it
    /// happens here.  On non-unix platforms, ddcommon uses `ring` instead, which handles this
    /// at rustls initialization. TODO: Move to the more ergonomic LazyLock when MSRV is 1.80
    /// In fips mode we expect someone to have done this already.
    #[cfg(not(feature = "fips"))]
    fn ensure_crypto_provider_initialized() {
        use std::sync::OnceLock;
        static INIT_CRYPTO_PROVIDER: OnceLock<()> = OnceLock::new();
        INIT_CRYPTO_PROVIDER.get_or_init(|| {
            #[cfg(unix)]
            #[allow(clippy::expect_used)]
            rustls::crypto::aws_lc_rs::default_provider()
                .install_default()
                .expect("Failed to install default CryptoProvider");
        });
    }

    // This this actually needs to be done by the user somewhere in their own main.
    #[cfg(feature = "fips")]
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
    pub(super) fn build_https_connector() -> anyhow::Result<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    > {
        ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed

        let certs = load_root_certs()?;
        let client_config = ClientConfig::builder()
            .with_root_certificates(certs)
            .with_no_client_auth();
        Ok(hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(client_config)
            .https_or_http()
            .enable_http1()
            .build())
    }

    #[cfg(not(feature = "use_webpki_roots"))]
    fn load_root_certs() -> anyhow::Result<rustls::RootCertStore> {
        use super::errors;

        let mut roots = rustls::RootCertStore::empty();

        let cert_result = rustls_native_certs::load_native_certs();
        if cert_result.certs.is_empty() {
            if let Some(err) = cert_result.errors.into_iter().next() {
                return Err(err.into());
            }
        }
        // TODO(paullgdfc): log errors even if there are valid certs, instead of ignoring them

        for cert in cert_result.certs {
            //TODO: log when invalid cert is loaded
            roots.add(cert).ok();
        }
        if roots.is_empty() {
            return Err(errors::Error::NoValidCertifacteRootsFound.into());
        }
        Ok(roots)
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
            #[cfg(feature = "https")]
            Connector::Https(c) => c.poll_ready(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::hyper_migration;
    use std::env;
    use tower_service::Service;

    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    /// Verify that the Connector type implements the correct bound Connect + Clone
    /// to be able to use the hyper::Client
    fn test_hyper_client_from_connector() {
        let _: hyper_migration::HttpClient = hyper_migration::new_default_client();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
    fn test_hyper_client_from_connector_with_webpki_roots() {
        let _: hyper_migration::HttpClient = hyper_migration::new_default_client();
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    /// Verify that Connector will only allow non tls connections if root certificates
    /// are not found
    async fn test_missing_root_certificates_only_allow_http_connections() {
        const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";
        const ENV_SSL_CERT_DIR: &str = "SSL_CERT_DIR";
        let old_value = env::var(ENV_SSL_CERT_FILE).unwrap_or_default();
        let old_dir_value = env::var(ENV_SSL_CERT_DIR).unwrap_or_default();

        env::set_var(ENV_SSL_CERT_FILE, "this/folder/does/not/exist");
        env::set_var(ENV_SSL_CERT_DIR, "this/folder/does/not/exist");
        let mut connector = Connector::new();

        assert!(matches!(connector, Connector::Http(_)));

        let stream = connector
            .call(hyper::Uri::from_static("https://example.com"))
            .await
            .unwrap_err();

        assert_eq!(
            *stream.downcast::<errors::Error>().unwrap(),
            errors::Error::CannotEstablishTlsConnection
        );

        env::set_var(ENV_SSL_CERT_FILE, old_value);
        env::set_var(ENV_SSL_CERT_DIR, old_dir_value);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
    #[cfg(feature = "https")]
    /// Verify that Connector will allow tls connections if root certificates
    /// are not found but can use webpki certificates
    async fn test_missing_root_certificates_use_webpki_certificates() {
        const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";
        let old_value = env::var(ENV_SSL_CERT_FILE).unwrap_or_default();

        env::set_var(ENV_SSL_CERT_FILE, "this/folder/does/not/exist");
        let mut connector = Connector::new();
        assert!(matches!(connector, Connector::Https(_)));

        let stream = connector
            .call(hyper::Uri::from_static("https://example.com"))
            .await;

        assert!(stream.is_ok());

        env::set_var(ENV_SSL_CERT_FILE, old_value);
    }
}
