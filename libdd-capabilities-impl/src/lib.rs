// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native capability implementations for libdatadog.
//!
//! `NativeCapabilities` is the bundle struct that implements all capability
//! traits using platform-native backends (hyper for HTTP, tokio for spawn,
//! etc.). Leaf crates (FFI, benchmarks) pin this type as the generic parameter.

mod http;
pub mod sleep;
pub mod spawn;

use core::future::Future;
use std::time::Duration;

pub use http::NativeHttpClient;
use libdd_capabilities::{http::HttpError, MaybeSend};
pub use libdd_capabilities::{HttpClientCapability, SleepCapability};
pub use sleep::NativeSleepCapability;
pub use spawn::NativeSpawnCapability; // kept for backwards compatibility

/// Bundle struct for native platform capabilities.
///
/// Delegates to [`NativeHttpClient`] for HTTP and [`NativeSleepCapability`] for
/// sleep. Task spawning is handled internally by `SharedRuntime`.
///
/// Individual capability traits keep minimal per-function bounds (e.g.
/// functions that only need HTTP require just `H: HttpClientCapability`, not the
/// full bundle) so that native callers like the sidecar can use
/// `NativeHttpClient` directly without pulling in this bundle.
#[derive(Clone, Debug)]
pub struct NativeCapabilities {
    http: NativeHttpClient,
    sleep: NativeSleepCapability,
}

impl Default for NativeCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeCapabilities {
    pub fn new() -> Self {
        Self {
            http: NativeHttpClient::new_client(),
            sleep: NativeSleepCapability,
        }
    }
}

impl HttpClientCapability for NativeCapabilities {
    fn new_client() -> Self {
        Self {
            http: NativeHttpClient::new_client(),
            sleep: NativeSleepCapability,
        }
    }

    fn request(
        &self,
        req: ::http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<::http::Response<bytes::Bytes>, HttpError>> + MaybeSend {
        self.http.request(req)
    }
}

impl SleepCapability for NativeCapabilities {
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend {
        self.sleep.sleep(duration)
    }
}
