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
/// traits are added (spawn, sleep, etc.), this type will become a proper struct
/// implementing all of them.
///
/// At that point, consider introducing a `CapabilitiesBundle` trait in
/// `libdd-capabilities` with a `fn new() -> Self` constructor, so that bundle
/// creation is decoupled from `HttpClientTrait::new_client()`. Individual
/// capability traits should keep minimal per-function bounds (e.g. functions
/// that only need HTTP should require just `H: HttpClientTrait`, not the full
/// bundle) as this lets native callers like the sidecar use `DefaultHttpClient`
/// directly without pulling in the full bundle.
pub type NativeCapabilities = DefaultHttpClient;
