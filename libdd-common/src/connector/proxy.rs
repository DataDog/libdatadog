// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::task::{Context, Poll};
use std::sync::LazyLock;

use futures::future::BoxFuture;
use futures::FutureExt;
use hyper_util::client::legacy::connect::{proxy::Tunnel, HttpConnector};
use hyper_util::client::proxy::matcher::Matcher;

use super::conn_stream::{ConnStream, ConnStreamError};
use super::{https, Connector};

// Read getenv only once at init to avoid concurrency issues with the environment.
static PROXY_MATCHER: LazyLock<Matcher> = LazyLock::new(Matcher::from_env);

/// Wraps a direct `Connector` to additionally honor the `HTTPS_PROXY`/`https_proxy`
/// and `NO_PROXY`/`no_proxy` environment variables.
#[derive(Clone)]
pub(super) struct HttpsProxyConnector {
    matcher: &'static Matcher,
    tls_config: Option<rustls::ClientConfig>,
    http: HttpConnector,
    direct: Connector,
}

impl HttpsProxyConnector {
    pub(super) fn new(direct: Connector) -> Self {
        Self {
            matcher: &PROXY_MATCHER,
            tls_config: https::build_tls_config().ok(),
            http: HttpConnector::new(),
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
        if require_tls {
            if let (Some(tls_config), Some(intercept)) =
                (&self.tls_config, self.matcher.intercept(&uri))
            {
                let mut tunnel = Tunnel::new(intercept.uri().clone(), self.http.clone());
                if let Some(auth) = intercept.basic_auth() {
                    tunnel = tunnel.with_auth(auth.clone());
                }

                let mut https = hyper_rustls::HttpsConnectorBuilder::new()
                    .with_tls_config(tls_config.clone())
                    .https_or_http()
                    .enable_http1()
                    .wrap_connector(tunnel);

                return ConnStream::from_https_connector_with_uri(&mut https, uri, true).boxed();
            }
        }

        tower_service::Service::call(&mut self.direct, uri)
    }
}
