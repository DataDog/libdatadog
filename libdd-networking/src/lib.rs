// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]
extern crate alloc;

#[cfg(all(feature = "reqwest", feature = "http-client"))]
pub mod dump_server;
pub mod endpoint;
#[cfg(feature = "http-client")]
pub mod http_common;

#[cfg(not(target_arch = "wasm32"))]
pub mod connector;

#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
pub trait Connect:
    hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static
{
}
#[cfg(all(not(target_arch = "wasm32"), feature = "http-client"))]
impl<C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static> Connect
    for C
{
}
