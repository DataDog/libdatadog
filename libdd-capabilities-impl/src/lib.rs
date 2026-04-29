// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native capability implementations for libdatadog.
//!
//! `NativeCapabilities` is the bundle struct that implements all capability
//! traits using platform-native backends (hyper for HTTP, tokio for spawn,
//! etc.). Leaf crates (FFI, benchmarks) pin this type as the generic parameter.

mod http;

pub use libdd_capabilities::HttpClientTrait;

#[cfg(not(target_arch = "wasm32"))]
pub use http::DefaultHttpClient;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use core::future::Future;

    use libdd_capabilities::http::HttpError;
    use libdd_capabilities::MaybeSend;

    use super::DefaultHttpClient;
    use super::HttpClientTrait;

    /// Bundle struct for native platform capabilities.
    ///
    /// Delegates to [`DefaultHttpClient`] for HTTP. As more capability traits are
    /// added (spawn, sleep, etc.), additional fields and impls are added here
    /// without changing the type identity — consumers see the same
    /// `NativeCapabilities` throughout.
    ///
    /// Individual capability traits keep minimal per-function bounds (e.g.
    /// functions that only need HTTP require just `H: HttpClientTrait`, not the
    /// full bundle) so that native callers like the sidecar can use
    /// `DefaultHttpClient` directly without pulling in this bundle.
    #[derive(Clone, Debug)]
    pub struct NativeCapabilities {
        http: DefaultHttpClient,
    }

    impl HttpClientTrait for NativeCapabilities {
        fn new_client() -> Self {
            Self {
                http: DefaultHttpClient::new_client(),
            }
        }

        fn request(
            &self,
            req: ::http::Request<bytes::Bytes>,
        ) -> impl Future<Output = Result<::http::Response<bytes::Bytes>, HttpError>> + MaybeSend
        {
            self.http.request(req)
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::NativeCapabilities;
