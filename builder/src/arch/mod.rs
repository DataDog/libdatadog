// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod linux;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub use crate::arch::linux::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "musl"))]
mod musl;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub use crate::arch::musl::*;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(target_os = "macos")]
pub mod apple;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[cfg(target_os = "macos")]
pub use crate::arch::apple::*;

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
#[cfg(target_os = "windows")]
pub use crate::arch::windows::*;
