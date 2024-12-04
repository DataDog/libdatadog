// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::future::BoxFuture;
use futures::{future, FutureExt};
use hyper::client::HttpConnector;

#[cfg(feature = "use_webpki_roots")]
use hyper_rustls::ConfigBuilderExt;

use lazy_static::lazy_static;

use rustls::ClientConfig;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(unix)]
pub mod uds;

pub mod named_pipe;

pub mod errors;

mod conn_stream;
use conn_stream::{ConnStream, ConnStreamError};

#[derive(Clone)]
pub enum Connector {
    Http(hyper::client::HttpConnector),
    Https(hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>),
}

lazy_static! {
    static ref DEFAULT_CONNECTOR: Connector = Connector::new();
}

// When using aws-lc-rs, rustls needs to be initialized with the default CryptoProvider; sometimes
// this is done as a side-effect of other operations, but we need to ensure it happens here.  On
// non-unix platforms, ddcommon uses `ring` instead, which handles this at rustls initialization.
#[cfg(feature = "use_webpki_roots")]
lazy_static! {
    static ref INIT_CRYPTO_PROVIDER: () = {
        #[cfg(unix)]
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("Failed to install default CryptoProvider");
    };
}

#[cfg(not(feature = "use_webpki_roots"))]
lazy_static! {
    static ref INIT_CRYPTO_PROVIDER: () = {
        #[cfg(unix)]
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install default CryptoProvider");
    };
}

impl Default for Connector {
    fn default() -> Self {
        DEFAULT_CONNECTOR.clone()
    }
}

impl Connector {
    pub fn new() -> Self {
        #[cfg(feature = "use_webpki_roots")]
        let https_connector_fn = build_https_connector_with_webpki_roots;
        #[cfg(not(feature = "use_webpki_roots"))]
        let https_connector_fn = build_https_connector;

        match https_connector_fn() {
            Ok(connector) => Connector::Https(connector),
            Err(_) => Connector::Http(HttpConnector::new()),
        }
    }

    fn build_conn_stream<'a>(
        &mut self,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> BoxFuture<'a, Result<ConnStream, ConnStreamError>> {
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
            Self::Https(c) => {
                ConnStream::from_https_connector_with_uri(c, uri, require_tls).boxed()
            }
        }
    }
}

#[cfg(not(feature = "use_webpki_roots"))]
fn build_https_connector(
) -> anyhow::Result<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>>
{
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

#[cfg(feature = "use_webpki_roots")]
fn build_https_connector_with_webpki_roots(
) -> anyhow::Result<hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>>
{
    *INIT_CRYPTO_PROVIDER; // One-time initialization of a crypto provider if needed

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
fn load_root_certs() -> anyhow::Result<rustls::RootCertStore> {
    *INIT_CRYPTO_PROVIDER; // One-time initialization of a crypto provider if needed
    let mut roots = rustls::RootCertStore::empty();

    for cert in rustls_native_certs::load_native_certs()? {
        //TODO: log when invalid cert is loaded
        roots.add(cert).ok();
    }
    if roots.is_empty() {
        return Err(errors::Error::NoValidCertifacteRootsFound.into());
    }
    Ok(roots)
}

impl hyper::service::Service<hyper::Uri> for Connector {
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
            Connector::Https(c) => c.poll_ready(cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use hyper::service::Service;
    use std::env;

    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    /// Verify that the Connector type implements the correct bound Connect + Clone
    /// to be able to use the hyper::Client
    fn test_hyper_client_from_connector() {
        let _: hyper::Client<Connector> = hyper::Client::builder().build(Connector::new());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
    fn test_hyper_client_from_connector_with_webpki_roots() {
        let _: hyper::Client<Connector> = hyper::Client::builder().build(Connector::new());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(not(feature = "use_webpki_roots"))]
    /// Verify that Connector will only allow non tls connections if root certificates
    /// are not found
    async fn test_missing_root_certificates_only_allow_http_connections() {
        const ENV_SSL_CERT_FILE: &str = "SSL_CERT_FILE";
        let old_value = env::var(ENV_SSL_CERT_FILE).unwrap_or_default();

        env::set_var(ENV_SSL_CERT_FILE, "this/folder/does/not/exist");
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
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(feature = "use_webpki_roots")]
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
