// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native capability implementations.
//!
//! This crate is patchable: `libdatadog-nodejs` overrides it via
//! `[patch.crates-io]` with wasm implementations backed by JS transports.
//! Both versions export the same `DefaultHttpClient` type.

mod http;

pub use http::DefaultHttpClient;
