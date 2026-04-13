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
use libdd_capabilities::http::HttpError;
pub use libdd_capabilities::HttpClientCapability;
use libdd_capabilities::MaybeSend;
pub use libdd_capabilities::SleepCapability;
pub use libdd_capabilities::SpawnCapability;
pub use sleep::NativeSleepCapability;
pub use spawn::{NativeJoinHandle, NativeSpawnCapability};

/// Bundle struct for native platform capabilities.
///
/// Delegates to [`NativeHttpClient`] for HTTP, [`NativeSleepCapability`] for
/// sleep, and [`NativeSpawnCapability`] for task spawning.
///
/// Individual capability traits keep minimal per-function bounds (e.g.
/// functions that only need HTTP require just `H: HttpClientCapability`, not the
/// full bundle) so that native callers like the sidecar can use
/// `NativeHttpClient` directly without pulling in this bundle.
#[derive(Clone, Debug)]
pub struct NativeCapabilities {
    http: NativeHttpClient,
    sleep: NativeSleepCapability,
    spawn: NativeSpawnCapability,
}

impl NativeCapabilities {
    /// Create a bundle with an explicit tokio runtime handle for spawning.
    ///
    /// Prefer `new_client()` (via `HttpClientCapability`) when already inside
    /// a tokio context. This constructor exists for test code that owns a
    /// `SharedRuntime` and needs to pass its handle explicitly.
    pub fn new(handle: tokio::runtime::Handle) -> Self {
        Self {
            http: NativeHttpClient::new_client(),
            sleep: NativeSleepCapability,
            spawn: NativeSpawnCapability::new(handle),
        }
    }
}

impl HttpClientCapability for NativeCapabilities {
    fn new_client() -> Self {
        Self {
            http: NativeHttpClient::new_client(),
            sleep: NativeSleepCapability,
            spawn: NativeSpawnCapability::from_current(),
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

impl SpawnCapability for NativeCapabilities {
    type JoinHandle<T: MaybeSend + 'static> = NativeJoinHandle<T>;

    fn spawn<F, T>(&self, future: F) -> NativeJoinHandle<T>
    where
        F: Future<Output = T> + MaybeSend + 'static,
        T: MaybeSend + 'static,
    {
        self.spawn.spawn(future)
    }
}
