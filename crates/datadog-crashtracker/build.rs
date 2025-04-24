// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
fn main() {
    cc::Build::new()
        .file("src/crash_info/emit_sicodes.c")
        .compile("emit_sicodes");
}

#[cfg(not(unix))]
fn main() {}
