// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub(crate) static ENV_PASS_FD_KEY: &str = "__DD_INTERNAL_PASSED_FD";

#[cfg(target_family = "unix")]
#[macro_use]
mod unix;

#[cfg(target_family = "unix")]
pub use unix::*;

#[cfg(target_os = "windows")]
mod win32;

#[cfg(target_os = "windows")]
pub use win32::*;
