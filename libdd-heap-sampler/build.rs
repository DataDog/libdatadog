// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    #[cfg(unix)]
    {
        let sources = [
            "src/allocation_requested.c",
            "src/allocation_created.c",
            "src/allocation_freed.c",
            "src/sample_flag.c",
            "src/tl_state.c",
        ];
        let headers = [
            "include/datadog/heap/allocation_requested.h",
            "include/datadog/heap/allocation_created.h",
            "include/datadog/heap/allocation_freed.h",
            "include/datadog/heap/sample_flag.h",
            "include/datadog/heap/tl_state.h",
        ];

        let mut build = cc::Build::new();
        build
            .files(sources)
            .include("include")
            .warnings(true)
            .flag_if_supported("-Wextra");
        build.compile("dd_heap_sampler");

        for f in sources.iter().chain(headers.iter()) {
            println!("cargo:rerun-if-changed={}", f);
        }
    }
}
