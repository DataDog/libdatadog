// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    #[cfg(unix)]
    unix::build();
}

#[cfg(unix)]
mod unix {
    use std::env;
    use std::path::PathBuf;

    const SOURCES: &[&str] = &[
        "src/allocation_requested.c",
        "src/allocation_created.c",
        "src/allocation_freed.c",
        "src/probes.c",
        "src/sample_flag.c",
        "src/tl_state.c",
    ];
    // Crate-relative so bindgen can open the files directly. The leading
    // `include/` gets stripped from the wrapper's `#include` lines below
    // (see `patch_wrapper_includes`) so it compiles against `-Iinclude`.
    const HEADERS: &[&str] = &[
        "include/datadog/heap/allocation_requested.h",
        "include/datadog/heap/allocation_created.h",
        "include/datadog/heap/allocation_freed.h",
        "include/datadog/heap/probes.h",
        "include/datadog/heap/sample_flag.h",
        "include/datadog/heap/tl_state.h",
    ];
    const INCLUDE_PREFIX_TO_STRIP: &str = "include/";

    pub fn build() {
        // Bindings first; bindgen writes the static-inline wrapper C
        // source, which the cc build then picks up alongside our own sources.
        let wrap_path = generate_bindings();
        compile_c(&wrap_path);
    }

    fn generate_bindings() -> PathBuf {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
        let wrap_path = out_dir.join("dd_heap_sampler_static_wrappers.c");

        let mut builder = bindgen::Builder::default()
            .clang_arg("-Iinclude")
            .allowlist_function("dd_.*")
            .allowlist_type("dd_.*")
            .allowlist_var("DD_.*")
            // Emit FFI-linkable shims for `static inline` helpers so we
            // have a single source of truth for the fast path (the C
            // header), reached via one function call from Rust.
            .wrap_static_fns(true)
            .wrap_static_fns_path(&wrap_path)
            .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

        for h in HEADERS {
            builder = builder.header(*h);
        }

        builder
            .generate()
            .expect("bindgen failed to generate bindings")
            .write_to_file(out_dir.join("bindings.rs"))
            .expect("failed to write bindings.rs");

        patch_wrapper_includes(&wrap_path);
        wrap_path
    }

    /// bindgen embeds the exact path string we passed to `.header()` into
    /// the wrapper's `#include "..."` lines. Our headers are opened as
    /// `include/datadog/heap/…` (crate-relative), but the wrapper needs to
    /// resolve them against `-Iinclude` when cc compiles it. Strip the
    /// leading `include/` from each quoted include so they match.
    fn patch_wrapper_includes(wrap_path: &PathBuf) {
        let source = std::fs::read_to_string(wrap_path)
            .expect("wrapper C file missing after bindgen");
        let needle = format!("\"{INCLUDE_PREFIX_TO_STRIP}");
        let patched = source.replace(&needle, "\"");
        std::fs::write(wrap_path, patched).expect("failed to rewrite wrapper C file");
    }

    fn compile_c(wrap_path: &PathBuf) {
        let mut build = cc::Build::new();
        build
            .files(SOURCES)
            .file(wrap_path)
            .include("include")
            .warnings(true)
            .flag_if_supported("-Wextra");
        build.compile("dd_heap_sampler");

        for f in SOURCES {
            println!("cargo:rerun-if-changed={f}");
        }
    }
}
