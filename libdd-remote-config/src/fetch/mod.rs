// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "agentless")]
mod agentless;

mod fetcher;
#[cfg(not(target_arch = "wasm32"))]
mod multitarget;
#[cfg(not(target_arch = "wasm32"))]
mod shared;
mod single;

#[cfg(any(test, feature = "test"))]
pub mod test_server;

#[cfg(feature = "agentless")]
pub use agentless::*;

#[allow(clippy::useless_attribute)] // different clippy versions are differently picky
#[cfg_attr(test, allow(ambiguous_glob_reexports))] // ignore mod tests re-export
pub use fetcher::*;
#[cfg(not(target_arch = "wasm32"))]
pub use multitarget::*;
#[cfg(not(target_arch = "wasm32"))]
pub use shared::*;
pub use single::*;

/// A random UUIDv4 string, used for the ids a fetcher reports itself under.
///
/// Workaround to avoid pulling in the "js" feature from uuid crate, which accesses
/// node APIs in not existing in older node versions.
pub(crate) fn random_uuid_string() -> String {
    let mut bytes = [0u8; 16];

    if let Err(error) = getrandom::getrandom(&mut bytes) {
        tracing::error!("No entropy source for the remote config client id: {error}");
    }

    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}
