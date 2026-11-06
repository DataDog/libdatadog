// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
extern crate alloc;

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "aix")))]
pub mod threading;
#[cfg(not(target_arch = "wasm32"))]
pub mod timeout;
pub mod unix_utils;
