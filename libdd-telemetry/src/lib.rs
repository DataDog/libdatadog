// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::mutex_atomic)]
#![allow(clippy::nonminimal_bool)]
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "std")]
pub mod config;
pub mod data;
#[cfg(feature = "std")]
pub mod info;
#[cfg(feature = "std")]
pub mod metrics;
#[cfg(feature = "std")]
pub mod worker;

#[cfg(feature = "std")]
pub use info::build_host;
#[cfg(feature = "alloc")]
pub use libdd_common::tag::{Tag, parse_tags};
#[cfg(feature = "std")]
pub use libdd_common::{Endpoint, parse_uri};
