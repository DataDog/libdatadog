// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(all(unix, feature = "collector"))]
mod collector;
mod crash_info;
#[cfg(feature = "demangler")]
mod demangler;
#[cfg(all(unix, feature = "receiver"))]
mod receiver;

#[cfg(all(unix, feature = "collector"))]
pub use collector::*;
pub use crash_info::*;
#[cfg(feature = "demangler")]
pub use demangler::*;
#[cfg(all(unix, feature = "receiver"))]
pub use receiver::*;
