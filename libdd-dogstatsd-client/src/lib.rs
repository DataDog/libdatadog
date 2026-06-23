// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! dogstatsd-client implements a client to emit metrics to a dogstatsd server.
//! This is made use of in at least the data-pipeline and sidecar crates.

/// Types representing dogstatsd actions
mod action;
pub use action::{DogStatsDAction, DogStatsDActionOwned};

/// Dogstatsd client used to send metrics
mod client;
pub use client::{new, Client};
