// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
#![deny(unsafe_op_in_unsafe_fn)]

mod assignment;
mod configuration;
mod evaluation_context;
mod handle;

pub use assignment::*;
pub use configuration::*;
pub use evaluation_context::*;
pub use handle::*;
