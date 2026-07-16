// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native capability implementations for libdatadog.
//!
//! `NativeCapabilities` is the bundle struct that implements all capability
//! traits using platform-native backends (hyper for HTTP, tokio for sleep,
//! etc.). Leaf crates (FFI, benchmarks) pin this type as the generic parameter.

pub mod env;
mod http;
pub mod sleep;

use core::future::Future;
use std::time::Duration;

pub use env::NativeEnvCapability;
pub use http::NativeHttpClient;
use libdd_capabilities::{http::HttpError, MaybeSend};
pub use libdd_capabilities::{
    EnvCapability, EnvError, HttpClientCapability, LogWriterCapability, SleepCapability,
};
pub use sleep::NativeSleepCapability;

/// Bundle struct for native platform capabilities.
///
/// Delegates to [`NativeHttpClient`] for HTTP and to unit-struct capabilities
/// for the rest. Task spawning is handled internally by `SharedRuntime`.
///
/// Individual capability traits keep minimal per-function bounds (e.g.
/// functions that only need HTTP require just `H: HttpClientCapability`, not the
/// full bundle) so that native callers like the sidecar can use
/// `NativeHttpClient` directly without pulling in this bundle.
#[derive(Clone, Debug)]
pub struct NativeCapabilities {
    http: NativeHttpClient,
    sleep: NativeSleepCapability,
    env: NativeEnvCapability,
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
            env: NativeEnvCapability,
        }
    }
}

impl HttpClientCapability for NativeCapabilities {
    fn new_client() -> Self {
        Self::new()
    }

    fn request(
        &self,
        req: ::http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<::http::Response<bytes::Bytes>, HttpError>> + MaybeSend {
        self.http.request(req)
    }
}

impl LogWriterCapability for NativeCapabilities {
    fn write_log_output(&self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        // `Stdout` is internally synchronized; lock once so the whole buffer
        // (one or more newline-terminated JSON lines) is written without
        // interleaving, then flush.
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

impl SleepCapability for NativeCapabilities {
    fn new() -> Self {
        Self::new()
    }

    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + MaybeSend {
        self.sleep.sleep(duration)
    }
}

impl EnvCapability for NativeCapabilities {
    fn new() -> Self {
        Self::new()
    }

    fn get(&self, name: &str) -> Result<Option<String>, EnvError> {
        self.env.get(name)
    }
}
