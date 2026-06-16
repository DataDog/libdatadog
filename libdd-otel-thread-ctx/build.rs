// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::env;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();

    if target_os != "linux" {
        return;
    }

    if !matches!(target_arch.as_str(), "x86_64" | "aarch64") {
        panic!(
            "Unsupported architecture `{}` for otel-thread-ctx on Linux. Only x86_64 and aarch64 are currently supported.",
            target_arch
        )
    }
}
