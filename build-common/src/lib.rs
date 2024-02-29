// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#[cfg(not(feature = "cbindgen"))]
pub fn generate_and_configure_header(_header_name: &str) {}

#[cfg(feature = "cbindgen")]
mod cbindgen;
#[cfg(feature = "cbindgen")]
pub use crate::cbindgen::*;
