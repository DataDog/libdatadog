// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "agentless")]
mod agentless;

mod fetcher;
mod multitarget;
mod shared;
mod single;

#[cfg(any(test, feature = "test"))]
pub mod test_server;

#[cfg(feature = "agentless")]
pub use agentless::*;

#[allow(clippy::useless_attribute)] // different clippy versions are differently picky
#[cfg_attr(test, allow(ambiguous_glob_reexports))] // ignore mod tests re-export
pub use fetcher::*;
pub use multitarget::*;
pub use shared::*;
pub use single::*;
