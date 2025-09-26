// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(all(unix, feature = "collector"))]
mod collector;
#[cfg(all(windows, feature = "collector_windows"))]
mod collector_windows;
mod crash_info;
#[cfg(feature = "demangler")]
mod demangler;
#[cfg(all(unix, feature = "receiver"))]
mod receiver;
mod runtime_callback;
#[cfg(all(unix, feature = "collector"))]
pub use collector::*;
#[cfg(all(windows, feature = "collector_windows"))]
pub use collector_windows::api::ddog_crasht_init_windows;
pub use crash_info::*;
#[cfg(feature = "demangler")]
pub use demangler::*;
#[cfg(all(unix, feature = "receiver"))]
pub use receiver::*;
pub use runtime_callback::*;
