// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Shared by both platforms; re-exported through each one's `sockets` module so that
/// `platform::sockets::PeerCredentials` resolves the same way it always has.
pub(crate) mod peer_credentials;

mod mem_handle;
pub use mem_handle::*;
mod platform_handle;
pub use platform_handle::*;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

mod message;
