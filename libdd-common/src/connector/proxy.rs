// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::task::{Context, Poll};
use std::sync::LazyLock;

use futures::FutureExt;
use futures::future::BoxFuture;
use hyper_util::client::legacy::connect::{HttpConnector, proxy::Tunnel};
use hyper_util::client::proxy::matcher::Matcher;

use super::Connector;
use super::conn_stream::{ConnStream, ConnStreamError};
#[cfg(feature = "tls-core")]
use super::https;

// Read getenv only once at init to avoid concurrency issues with the environment.
static PROXY_MATCHER: LazyLock<Matcher> = LazyLock::new(Matcher::from_env);

/// Wraps a direct `Connector` to additionally honor the `HTTP(S)_PROXY`/`http(s)_proxy`
/// and `NO_PROXY`/`no_proxy` environment variables.
#[derive(Clone)]
pub(super) struct HttpProxyConnector {
    matcher: &'static Matcher,
    #[cfg(feature = "tls-core")]
    tls_config: Option<rustls::ClientConfig>,
    #[cfg(feature = "tls-core")]
    proxy_dialer: Option<hyper_rustls::HttpsConnector<HttpConnector>>,
    direct: Connector,
}

impl HttpProxyConnector {
    pub(super) fn new(direct: Connector) -> Self {
        #[cfg(feature = "tls-core")]
        let tls_config = https::build_tls_config().ok();
        #[cfg(feature = "tls-core")]
        let proxy_dialer = tls_config.clone().map(|tls_config| {
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_or_http()
                .enable_http1()
                .build()
        });
        Self {
            matcher: &PROXY_MATCHER,
            #[cfg(feature = "tls-core")]
            tls_config,
            #[cfg(feature = "tls-core")]
            proxy_dialer,
            direct,
        }
    }

    pub(super) fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), ConnStreamError>> {
        tower_service::Service::poll_ready(&mut self.direct, cx)
    }

    pub(super) fn build_conn_stream(
        &mut self,
        uri: hyper::Uri,
        require_tls: bool,
    ) -> BoxFuture<'static, Result<ConnStream, ConnStreamError>> {
        let Some(intercept) = self.matcher.intercept(&uri) else {
            return tower_service::Service::call(&mut self.direct, uri);
        };

        // Tunnel defaults to 443 when the destination omits a port, even for
        // HTTP. Normalize only the tunnel destination; proxy matching and the
        // request URI retain their original scheme, host and path.
        let uri = match tunnel_destination(uri) {
            Ok(uri) => uri,
            Err(error) => return futures::future::ready(Err(error.into())).boxed(),
        };

        #[cfg(feature = "tls-core")]
        if let (Some(tls_config), Some(proxy_dialer)) = (&self.tls_config, &self.proxy_dialer) {
            let mut tunnel = Tunnel::new(intercept.uri().clone(), proxy_dialer.clone());
            if let Some(auth) = intercept.basic_auth() {
                tunnel = tunnel.with_auth(auth.clone());
            }

            let mut https = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config.clone())
                .https_or_http()
                .enable_http1()
                .wrap_connector(tunnel);

            return ConnStream::from_https_connector_with_uri(&mut https, uri, require_tls).boxed();
        }

        // No crypto provider is available, proxy only via http
        if !require_tls && intercept.uri().scheme_str() == Some("http") {
            let mut tunnel = Tunnel::new(intercept.uri().clone(), HttpConnector::new());
            if let Some(auth) = intercept.basic_auth() {
                tunnel = tunnel.with_auth(auth.clone());
            }
            return ConnStream::from_tunnel_with_uri(&mut tunnel, uri).boxed();
        }

        tower_service::Service::call(&mut self.direct, uri)
    }
}

fn tunnel_destination(uri: hyper::Uri) -> Result<hyper::Uri, http::Error> {
    if uri.scheme_str() != Some("http") || uri.port_u16().is_some() {
        return Ok(uri);
    }
    let mut parts = uri.into_parts();
    if let Some(authority) = &parts.authority {
        parts.authority = Some(format!("{authority}:80").parse()?);
    }
    Ok(hyper::Uri::from_parts(parts)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn proxy_connect_preserves_scheme_ports_and_authentication() {
        for (destination, expected) in [
            ("http://cdn.example/config", "cdn.example:80"),
            ("http://cdn.example:8080/config", "cdn.example:8080"),
            ("http://[::1]/config", "[::1]:80"),
            ("https://cdn.example/config", "cdn.example:443"),
            ("https://cdn.example:8443/config", "cdn.example:8443"),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let matcher = Box::leak(Box::new(
                Matcher::builder()
                    .all(format!(
                        "http://user:password@{}",
                        listener.local_addr().unwrap()
                    ))
                    .build(),
            ));
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                stream
                    .write_all(b"HTTP/1.1 502 Test Complete\r\n\r\n")
                    .await
                    .unwrap();
                String::from_utf8(request).unwrap()
            });
            let mut connector = HttpProxyConnector::new(Connector::new_no_proxy());
            connector.matcher = matcher;
            assert!(
                connector
                    .build_conn_stream(
                        destination.parse().unwrap(),
                        destination.starts_with("https")
                    )
                    .await
                    .is_err()
            );
            let request = server.await.unwrap();
            assert!(
                request.starts_with(&format!("CONNECT {expected} HTTP/1.1\r\n")),
                "{request}"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("proxy-authorization: basic dxnlcjpwyxnzd29yza==\r\n")
            );
        }
    }

    #[tokio::test]
    async fn no_proxy_connects_directly() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let matcher = Box::leak(Box::new(
            Matcher::builder()
                .all("http://127.0.0.1:1")
                .no("127.0.0.1")
                .build(),
        ));
        let mut connector = HttpProxyConnector::new(Connector::new_no_proxy());
        connector.matcher = matcher;
        let uri = format!("http://{}/config", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        assert!(connector.build_conn_stream(uri, false).await.is_ok());
    }
}
