// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::task::{Context, Poll};
use std::sync::LazyLock;

use futures::future::BoxFuture;
use futures::FutureExt;
use hyper_util::client::legacy::connect::{proxy::Tunnel, HttpConnector};
use hyper_util::client::proxy::matcher::Matcher;

use super::conn_stream::{ConnStream, ConnStreamError};
#[cfg(feature = "tls-core")]
use super::https;
use super::Connector;

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
    /// Fails when the process-wide crypto provider cannot satisfy this build, the
    /// same condition [`Connector::try_cached`] reports. A missing trust store only
    /// leaves the proxy without a TLS config, which the CONNECT path handles.
    pub(super) fn try_new(direct: Connector) -> anyhow::Result<Self> {
        #[cfg(feature = "tls-core")]
        let tls_config = match https::build_tls_config() {
            Ok(config) => Some(config),
            // Only the caller can fix this; tunnelling over a provider this build
            // rejects would defeat the point of asking for it.
            Err(err @ https::TlsConfigError::CryptoProvider(_)) => {
                return Err(anyhow::anyhow!(err))
            }
            #[cfg(not(feature = "use_webpki_roots"))]
            Err(https::TlsConfigError::TrustRoots(_)) => None,
        };
        #[cfg(feature = "tls-core")]
        let proxy_dialer = tls_config.clone().map(|tls_config| {
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_or_http()
                .enable_http1()
                .build()
        });
        Ok(Self {
            matcher: &PROXY_MATCHER,
            #[cfg(feature = "tls-core")]
            tls_config,
            #[cfg(feature = "tls-core")]
            proxy_dialer,
            direct,
        })
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
