// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "http-client")]
use futures::future::BoxFuture;
#[cfg(feature = "http-client")]
use futures::{future, FutureExt};
#[cfg(feature = "http-client")]
use hyper_util::client::legacy::connect;

#[cfg(feature = "http-client")]
use core::future::Future;
#[cfg(feature = "http-client")]
use core::pin::Pin;
#[cfg(feature = "http-client")]
use core::task::{Context, Poll};
#[cfg(feature = "http-client")]
use std::sync::LazyLock;

#[cfg(unix)]
pub mod uds;

pub mod named_pipe;

pub mod errors;

#[cfg(feature = "http-client")]
mod conn_stream;
#[cfg(feature = "http-client")]
use conn_stream::{ConnStream, ConnStreamError};

#[cfg(feature = "hyper-proxy")]
mod proxy;

#[cfg(feature = "http-client")]
#[derive(Clone)]
// `proxy::HttpProxyConnector` is crate internal, and the field anyway not pub.
#[allow(private_interfaces)]
pub enum Connector {
    Http(connect::HttpConnector),
    #[cfg(feature = "tls-core")]
    Https(hyper_rustls::HttpsConnector<connect::HttpConnector>),
    #[cfg(feature = "hyper-proxy")]
    Proxy(Box<proxy::HttpProxyConnector>),
}

/// The cached connector, or the reason it could not be built.
///
/// `anyhow::Error` is not `Clone`, so the failure is cached as a string and
/// rebuilt per caller — the same approach `libdd-profiling`'s TLS cache uses.
#[cfg(feature = "http-client")]
static DEFAULT_CONNECTOR: LazyLock<Result<Connector, String>> =
    LazyLock::new(|| Connector::try_new().map_err(|err| err.to_string()));

#[cfg(feature = "http-client")]
impl Connector {
    /// Returns the process-wide cached connector.
    ///
    /// Fetching root certificates is expensive, so the connector is built once and
    /// cloned; this is the entry point callers should use.
    ///
    /// Fails when the process-wide crypto provider cannot satisfy this build, which
    /// a caller has to fix — most importantly a FIPS build that lost the provider
    /// install race to a non-FIPS provider. A missing trust store is *not* a failure
    /// here: TLS is then unavailable but plain HTTP still works, so the connector
    /// builds and an `https://` request later reports
    /// [`errors::Error::CannotEstablishTlsConnection`].
    pub fn try_cached() -> anyhow::Result<Self> {
        DEFAULT_CONNECTOR
            .as_ref()
            .map(Clone::clone)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    /// The cached connector, degrading to an HTTP-only connector when it could not
    /// be built.
    ///
    /// For callers that cannot surface an error. Prefer [`Self::try_cached`], which
    /// reports a crypto-provider misconfiguration instead of hiding it behind a
    /// connector that will refuse every `https://` request.
    pub(crate) fn cached_or_http_only() -> Self {
        Self::try_cached().unwrap_or_else(|_| Connector::Http(connect::HttpConnector::new()))
    }

    /// Make sure this function is not called frequently. Fetching the root certificates is an
    /// expensive operation. Access the globally cached connector via [`Self::try_cached`].
    fn try_new() -> anyhow::Result<Self> {
        #[cfg(feature = "hyper-proxy")]
        {
            Ok(Connector::Proxy(Box::new(
                proxy::HttpProxyConnector::try_new(Self::try_new_no_proxy()?)?,
            )))
        }
        #[cfg(not(feature = "hyper-proxy"))]
        {
            Self::try_new_no_proxy()
        }
    }

    pub(super) fn try_new_no_proxy() -> anyhow::Result<Self> {
        #[cfg(feature = "tls-core")]
        {
            match https::build_https_connector() {
                Ok(connector) => Ok(Connector::Https(connector)),
                // Only the caller can fix this, so it must not be swallowed.
                Err(err @ https::TlsConfigError::CryptoProvider(_)) => Err(anyhow::anyhow!(err)),
                // No trust store: keep plain HTTP working, as minimal containers
                // without CA certificates rely on.
                #[cfg(not(feature = "use_webpki_roots"))]
                Err(https::TlsConfigError::TrustRoots(_)) => {
                    Ok(Connector::Http(connect::HttpConnector::new()))
                }
            }
        }
        #[cfg(not(feature = "tls-core"))]
        {
            Ok(Connector::Http(connect::HttpConnector::new()))
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
            #[cfg(feature = "hyper-proxy")]
            Self::Proxy(p) => p.build_conn_stream(uri, require_tls),
        }
    }
}

#[cfg(feature = "tls-core")]
mod https {
    #[cfg(feature = "use_webpki_roots")]
    use hyper_rustls::ConfigBuilderExt;

    use rustls::ClientConfig;

    /// Ensures the rustls default CryptoProvider is installed: ring for non-FIPS.
    /// A provider the caller installed first is left in place.
    ///
    /// `https` and `fips` are alternatives, but Cargo features are additive and
    /// unification can enable both. Gating on `not(feature = "fips")` makes `fips`
    /// win at compile time, so the provider does not depend on whether this runs
    /// before or after the caller's own `install_default`.
    #[cfg(all(feature = "https", not(feature = "fips")))]
    fn ensure_crypto_provider_initialized() {
        use std::sync::Once;

        static INIT_CRYPTO_PROVIDER: Once = Once::new();

        INIT_CRYPTO_PROVIDER.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    /// In FIPS mode the provider is aws-lc-rs, which the `fips` feature builds in
    /// FIPS mode. Installing it here keeps the choice deterministic when `https` is
    /// also enabled and both providers are linked: rustls cannot derive one from its
    /// crate features then, and `ClientConfig::builder` would panic. A provider the
    /// caller installed first is left in place.
    #[cfg(feature = "fips")]
    fn ensure_crypto_provider_initialized() {
        use std::sync::Once;

        static INIT_CRYPTO_PROVIDER: Once = Once::new();

        INIT_CRYPTO_PROVIDER.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    /// Without a provider feature there is nothing to install; `require_crypto_provider`
    /// then insists the caller installed one.
    #[cfg(not(any(feature = "https", feature = "fips")))]
    fn ensure_crypto_provider_initialized() {}

    /// The install above is best-effort: the process-wide slot can only be filled
    /// once, `https` may also link ring, and any other crate in the process can
    /// install it first. A FIPS build must not fall back to whatever won that race,
    /// so the provider is checked rather than assumed.
    #[cfg(feature = "fips")]
    fn require_crypto_provider() -> Result<(), TlsConfigError> {
        match rustls::crypto::CryptoProvider::get_default() {
            Some(provider) if provider.fips() => Ok(()),
            Some(_) => Err(TlsConfigError::CryptoProvider(
                "a non-FIPS rustls CryptoProvider is already installed process-wide, so this \
                 FIPS build cannot use it; install the FIPS provider before anything else \
                 installs one"
                    .to_owned(),
            )),
            None => Err(TlsConfigError::CryptoProvider(
                "no rustls CryptoProvider could be installed for this FIPS build".to_owned(),
            )),
        }
    }

    /// Non-FIPS builds accept any installed provider — the ring install above, or
    /// whatever another crate installed first — but there must be one, because
    /// `ClientConfig::builder` panics when it cannot pick a provider and this crate
    /// is reached through FFI, where a panic aborts the host process.
    ///
    /// Without a provider feature nothing is installed above, so this is the check
    /// that holds such a build to supplying its own.
    #[cfg(not(feature = "fips"))]
    fn require_crypto_provider() -> Result<(), TlsConfigError> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            return Err(TlsConfigError::CryptoProvider(
                "no rustls CryptoProvider installed".to_owned(),
            ));
        }
        Ok(())
    }

    /// Why a TLS configuration could not be built.
    ///
    /// The two variants get different treatment by the connector, so they must stay
    /// distinguishable: one is a misconfiguration only the caller can fix, the other
    /// is an environment limitation that plain HTTP still works around.
    #[derive(Debug)]
    pub(super) enum TlsConfigError {
        /// The process-wide crypto provider cannot satisfy this build — a FIPS build
        /// that lost the install race to a non-FIPS provider, or no provider at all.
        /// Degrading to plain HTTP would hide a misconfiguration, so this propagates
        /// to whoever is constructing the connector.
        CryptoProvider(String),

        /// Trust roots could not be loaded. TLS is unavailable, but plain HTTP still
        /// works and minimal containers without CA certificates depend on that, so
        /// the connector degrades rather than failing to build.
        ///
        /// Cannot occur with `use_webpki_roots`, which compiles the roots in, so the
        /// variant does not exist there and every match stays exhaustive per build.
        #[cfg(not(feature = "use_webpki_roots"))]
        TrustRoots(String),
    }

    impl core::fmt::Display for TlsConfigError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::CryptoProvider(msg) => write!(f, "{msg}"),
                #[cfg(not(feature = "use_webpki_roots"))]
                Self::TrustRoots(msg) => write!(f, "could not load trust roots: {msg}"),
            }
        }
    }

    #[cfg(feature = "use_webpki_roots")]
    pub(super) fn build_tls_config() -> Result<ClientConfig, TlsConfigError> {
        ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed
        require_crypto_provider()?;

        Ok(ClientConfig::builder()
            .with_webpki_roots()
            .with_no_client_auth())
    }

    #[cfg(not(feature = "use_webpki_roots"))]
    /// Builds the client TLS config using the system trust roots.
    /// `SSL_CERT_FILE` and `SSL_CERT_DIR` variable are only supported on linux, see
    /// `rustls_platform_verifier` doc for details.
    pub(super) fn build_tls_config() -> Result<ClientConfig, TlsConfigError> {
        use rustls_platform_verifier::BuilderVerifierExt;

        ensure_crypto_provider_initialized(); // One-time initialization of a crypto provider if needed
        require_crypto_provider()?;

        Ok(ClientConfig::builder()
            .with_platform_verifier()
            .map_err(|err| TlsConfigError::TrustRoots(err.to_string()))?
            .with_no_client_auth())
    }

    pub(super) fn build_https_connector() -> Result<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        TlsConfigError,
    > {
        Ok(hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(build_tls_config()?)
            .https_or_http()
            .enable_http1()
            .build())
    }
}

#[cfg(feature = "http-client")]
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
            #[cfg(feature = "hyper-proxy")]
            Connector::Proxy(p) => p.poll_ready(cx),
        }
    }
}

#[cfg(all(test, feature = "http-client"))]
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
        let connector = Connector::try_new_no_proxy().unwrap();

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
        let connector = Connector::try_new_no_proxy().unwrap();
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
