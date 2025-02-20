// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(all(unix, feature = "collector"))]
mod collector;
#[cfg(all(windows, feature = "collector"))]
mod collector_windows;
mod crash_info;
#[cfg(feature = "demangler")]
mod demangler;
#[cfg(all(unix, feature = "receiver"))]
mod receiver;
#[cfg(all(unix, feature = "collector"))]
pub use collector::*;
#[cfg(all(windows, feature = "collector"))]
pub use collector_windows::api::ddog_crasht_init_windows;
pub use crash_info::*;
#[cfg(feature = "demangler")]
pub use demangler::*;
#[cfg(all(unix, feature = "receiver"))]
pub use receiver::*;
