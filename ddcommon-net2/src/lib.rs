// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Provides _some_ migration paths from the ddcommon
pub mod compat;

/// The http module has types and functions for working with HTTP requests
/// through hyper and tokio. Generally, we do not need asynchronous execution,
/// but we do need features like HTTP over UNIX Domain Sockets (UDS) and
/// Windows Named Pipes. This aims to provide a simple API for doing blocking,
/// synchronous HTTP calls with all the different connectors we support.
pub mod http;

/// This module exports some dependencies so that crates depending on this
/// one do not also have to directly depend on and manage the versions.
pub mod dep {
    pub use hex;
    pub use hyper::{self, http};
    pub use tokio;
    pub use tokio_rustls::{self, rustls};
}

pub mod crytpo {
    use std::sync::Arc;
    use tokio_rustls::rustls;

    #[derive(Clone, Debug)]
    pub enum Provider {
        #[cfg(feature = "use_native_roots")]
        Native(Arc<rustls::crypto::CryptoProvider>),

        #[cfg(feature = "use_webpki_roots")]
        Webpki(Arc<rustls::crypto::CryptoProvider>),

        // This won't work for TLS.
        None,
    }

    impl Provider {
        pub const fn none() -> Self {
            Provider::None
        }

        #[cfg(feature = "use_webpki_roots")]
        pub fn use_webpki_roots() -> Provider {
            Provider::Webpki(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        }

        #[cfg(feature = "use_native_roots")]
        pub fn use_native_roots() -> Provider {
            Provider::Native(Arc::new(rustls::crypto::ring::default_provider()))
        }

        /// Create the [rustls::ClientConfig] for the provider. Note that this
        /// can be expensive and should be called sparingly.
        pub fn get_client_config(&self) -> Option<Arc<rustls::ClientConfig>> {
            let mut cert_store = rustls::RootCertStore::empty();

            let maybe_provider: Option<Arc<rustls::crypto::CryptoProvider>> = match self {
                #[cfg(feature = "use_native_roots")]
                Provider::Native(crypto_provider) => {
                    // Its docs say:
                    // > This function can be expensive: on some platforms it
                    // > involves loading and parsing a ~300KB disk file.  It's
                    // > therefore prudent to call this sparingly.
                    let certs = rustls_native_certs::load_native_certs().certs;

                    // It is unlikely that the end-user selects HTTPs, so there's
                    // no need to hard-error here if certificates aren't added.
                    _ = cert_store.add_parsable_certificates(certs);
                    Some(crypto_provider.clone())
                }

                #[cfg(feature = "use_webpki_roots")]
                Provider::Webpki(crypto_provider) => {
                    cert_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                    Some(crypto_provider.clone())
                }

                Provider::None => None,
            };

            maybe_provider.and_then(|provider| {
                let client_config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_protocol_versions(rustls::DEFAULT_VERSIONS)
                    .ok()?
                    .with_root_certificates(cert_store)
                    .with_no_client_auth();
                Some(Arc::new(client_config))
            })
        }
    }
}

/// Holds a function to create a Tokio Runtime for the current thread.
/// Note that currently it will still use a thread pool for certain operations
/// which block.
pub mod rt {
    use std::io;
    use tokio::runtime;
    use tokio_util::sync::CancellationToken;

    /// Creates a tokio runtime for the current thread. This is the expected
    /// way to create a runtime used by this crate.
    pub fn create_current_thread_runtime() -> io::Result<runtime::Runtime> {
        runtime::Builder::new_current_thread().enable_all().build()
    }

    pub fn create_cancellation_token() -> CancellationToken {
        CancellationToken::new()
    }
}
