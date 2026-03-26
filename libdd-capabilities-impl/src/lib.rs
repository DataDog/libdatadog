// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native capability implementations for libdatadog.
//!
//! `NativeCapabilities` is the bundle struct that implements all capability
//! traits using platform-native backends (hyper for HTTP, tokio for spawn,
//! etc.). Leaf crates (FFI, benchmarks) pin this type as the generic parameter.

mod http;

pub use http::DefaultHttpClient;
pub use libdd_capabilities::HttpClientTrait;

/// Bundle struct for native platform capabilities.
///
/// Currently delegates to `DefaultHttpClient` for HTTP. As more capability
/// traits are added (spawn, sleep, etc.), this type will implement all of them.
pub type NativeCapabilities = DefaultHttpClient;
