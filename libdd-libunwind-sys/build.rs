// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_os = "linux")]
#[path = "buildscript/linux.rs"]
mod build;

#[cfg(target_os = "windows")]
#[path = "buildscript/windows.rs"]
mod build;

#[cfg(target_os = "macos")]
#[path = "buildscript/macos.rs"]
mod build;

fn main() {
    build::main();
}
