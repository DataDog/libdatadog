// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Platform capability provider.
//!
//! Re-exports the correct implementation of each capability trait based on
//! compile target. Downstream code uses these types directly, avoiding
//! generic contagion.

pub use libdd_capabilities::{HttpClientTrait, HttpError, HttpRequest, HttpResponse};

#[cfg(not(target_arch = "wasm32"))]
pub use libdd_common::capabilities::HyperHttpClient as DefaultHttpClient;

#[cfg(target_arch = "wasm32")]
pub use capabilities::WasmHttpClient as DefaultHttpClient;
