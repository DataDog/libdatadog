// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(feature = "cbindgen"))]
pub fn generate_and_configure_header(_header_name: &str) {}
#[cfg(not(feature = "cbindgen"))]
pub fn copy_and_configure_headers() {}

#[cfg(feature = "cbindgen")]
mod cbindgen;
#[cfg(feature = "cbindgen")]
pub use crate::cbindgen::*;
