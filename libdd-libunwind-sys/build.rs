// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(target_os = "linux")]
#[path = "build/linux.rs"]
mod build;

#[cfg(target_os = "windows")]
#[path = "build/windows.rs"]
mod build;

#[cfg(target_os = "macos")]
#[path = "build/macos.rs"]
mod build;

fn main() {
    build::main();
}
