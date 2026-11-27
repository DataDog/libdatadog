// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(target_os = "wasi")]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod api;
mod client;
mod host;
mod view;

include!(concat!(env!("OUT_DIR"), "/exports.rs"));
