// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg_attr(target_os = "linux", path = "build/linux.rs")]
mod build;

fn main() {
    build::main();
}

