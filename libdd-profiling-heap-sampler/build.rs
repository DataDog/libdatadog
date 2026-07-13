// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Build script for `libdd-profiling-heap-sampler`.
//!
//! On Linux, compiles the C sampler primitives under `src/*.c` and either
//! stages the checked-in bindgen artifacts from `src/generated/` into
//! `OUT_DIR` (default) or regenerates them from the public headers under
//! `include/datadog/heap/` when `LIBDD_PROFILING_HEAP_SAMPLER_REGEN` is set.
//! On every other target this is a no-op and the crate compiles to an
//! empty rlib.

fn main() {
    // USDT/SystemTap (sys/sdt.h) is Linux-only, so the crate compiles to an
    // empty rlib on every other target. Build scripts are compiled for the
    // host, so use Cargo's target cfg env var rather than #[cfg(target_os)].
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        linux::build();
    }
}

mod linux {
    use std::env;
    use std::path::{Path, PathBuf};

    const SOURCES: &[&str] = &[
        "src/allocation_requested.c",
        "src/allocation_created.c",
        "src/allocation_freed.c",
        "src/allocation_realloc.c",
        "src/probes.c",
        "src/sample_flag.c",
        "src/tl_state.c",
    ];

    // Checked-in bindgen outputs. Regenerated on demand by setting the
    // `LIBDD_PROFILING_HEAP_SAMPLER_REGEN` env var (see below); the default build
    // path only *reads* these files, so libclang is NOT a build-time
    // dependency for downstream consumers.
    //
    // The C implementation is still architecture-specific where needed
    // (`sample_flag.h` selects x86_64 header-magic vs aarch64 TBI at C
    // compile time), but the Rust-facing ABI intentionally excludes
    // those internal helpers/constants and is arch-independent.
    const GENERATED_BINDINGS: &str = "src/generated/bindings.rs";
    const GENERATED_WRAPPER: &str = "src/generated/dd_heap_sampler_static_wrappers.c";

    pub fn build() {
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
        let out_bindings = out_dir.join("bindings.rs");
        let out_wrapper = out_dir.join("dd_heap_sampler_static_wrappers.c");

        // Regeneration is opt-in via env var, NOT a cargo feature.
        // Cargo has no way to hide a feature from `--all-features`, and
        // most CI jobs in this workspace run with `--all-features`,
        // which would otherwise silently invoke bindgen on runners that
        // lack libclang. An env var is invisible to
        // `--all-features`, so this stays a deliberate opt-in for the
        // regen workflow only.
        println!("cargo:rerun-if-env-changed={REGEN_ENV_VAR}");
        if env::var_os(REGEN_ENV_VAR).is_some() {
            regen::run();
        }
        // Whether we just regenerated or not, we still need the checked-in
        // files staged into OUT_DIR so that lib.rs's
        // `include!(concat!(env!("OUT_DIR"), "/bindings.rs"))` finds
        // them and cc compiles the wrapper.
        use_checked_in(&out_bindings, &out_wrapper);

        compile_c(&out_wrapper);
    }

    /// Setting this env var to any value triggers a bindgen regen under
    /// `src/generated/` on the next build.rs invocation. Requires libclang;
    /// see `libdd-profiling-heap-sampler/README.md`.
    const REGEN_ENV_VAR: &str = "LIBDD_PROFILING_HEAP_SAMPLER_REGEN";

    fn use_checked_in(out_bindings: &Path, out_wrapper: &Path) {
        for src in [GENERATED_BINDINGS, GENERATED_WRAPPER] {
            println!("cargo:rerun-if-changed={src}");
            assert!(
                Path::new(src).is_file(),
                "checked-in bindgen output `{src}` missing. Run \
                 `{REGEN_ENV_VAR}=1 cargo build -p libdd-profiling-heap-sampler` \
                 to regenerate (requires libclang; see README)."
            );
        }
        std::fs::copy(GENERATED_BINDINGS, out_bindings)
            .expect("failed to stage checked-in bindings.rs into OUT_DIR");
        std::fs::copy(GENERATED_WRAPPER, out_wrapper)
            .expect("failed to stage checked-in wrapper .c into OUT_DIR");
    }

    fn compile_c(wrap_path: &Path) {
        // Translate the `live-heap` cargo feature into a C define.
        let live_heap = env::var_os("CARGO_FEATURE_LIVE_HEAP").is_some();

        let mut build = cc::Build::new();
        build
            .files(SOURCES)
            .file(wrap_path)
            .define("DD_HEAP_LIVE_TRACKING", if live_heap { "1" } else { "0" })
            .include(".")
            .include("include")
            // See regen::run's matching `-Ivendor` and vendor/README.md.
            .include("vendor")
            .warnings(true)
            .flag_if_supported("-Wextra")
            .flag_if_supported("-fcf-protection=none")
            // Use TLSDESC for dd_tl_state_storage. For static builds the
            // linker relaxes this to local-exec automatically. For dynamic
            // loads it works on both glibc and musl without allocation
            // concerns (see tl_state.h for the full analysis).
            .flag_if_supported("-mtls-dialect=gnu2");
        build.compile("dd_heap_sampler");

        // `allocation_requested.c` calls `log()`; glibc keeps it in libm,
        // separate from libc. On musl, math functions live in libc itself
        // and its stub libm.a is empty — but on a glibc host targeting musl,
        // an explicit -lm resolves to glibc's libm which fails to link.
        let target = env::var("TARGET").unwrap_or_default();
        if !target.contains("musl") {
            println!("cargo:rustc-link-lib=m");
        }

        for f in SOURCES {
            println!("cargo:rerun-if-changed={f}");
        }
    }

    mod regen {
        use super::{GENERATED_BINDINGS, GENERATED_WRAPPER};
        use std::path::Path;

        const HEADERS: &[&str] = &[
            "include/datadog/heap/allocation_requested.h",
            "include/datadog/heap/allocation_created.h",
            "include/datadog/heap/allocation_freed.h",
            "include/datadog/heap/allocation_realloc.h",
            "include/datadog/heap/probes.h",
            "include/datadog/heap/sample_flag.h",
            "include/datadog/heap/tl_state.h",
        ];

        /// Regenerate the checked-in bindings. The allowlist names only the
        /// Rust-facing ABI, so bindgen ignores architecture-specific internal
        /// constants/helpers from sample_flag.h and the output is reusable on
        /// both supported Linux architectures.
        pub fn run() {
            regen_bindings();
        }

        fn regen_bindings() {
            let committed_bindings = GENERATED_BINDINGS;
            let committed_wrapper = GENERATED_WRAPPER;
            if let Some(parent) = Path::new(committed_bindings).parent() {
                std::fs::create_dir_all(parent).expect("failed to create src/generated/");
            }

            let mut builder = bindgen::Builder::default()
                .clang_arg("-Iinclude")
                // `-Ivendor` makes <usdt.h> resolve to the vendored
                // libbpf/usdt single-header (BSD-2-Clause, see
                // vendor/README.md). We bundle it unconditionally rather
                // than depending on the system's `systemtap-sdt-dev`
                // package because libbpf/usdt is genuinely standalone
                // (no sdt-config.h companion file) and so works
                // identically across glibc and musl distros.
                .clang_arg("-Ivendor")
                .allowlist_function("dd_.*")
                .allowlist_type("dd_.*")
                .allowlist_var("DD_SAMPLING_INTERVAL_DEFAULT")
                // Emit FFI-linkable shims for `static inline` helpers so
                // we have a single source of truth for the fast path
                // (the C header), reached via one function call from
                // Rust.
                .wrap_static_fns(true)
                .wrap_static_fns_path(committed_wrapper)
                .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

            for h in HEADERS {
                println!("cargo:rerun-if-changed={h}");
                builder = builder.header(*h);
            }

            builder
                .generate()
                .unwrap_or_else(|e| panic!("bindgen failed to generate bindings: {e}"))
                .write_to_file(committed_bindings)
                .unwrap_or_else(|e| panic!("failed to write {committed_bindings}: {e}"));

            // bindgen doesn't emit any copyright header on its output;
            // libdatadog's CI runs `licensecheck` against every `.rs`/`.c`
            // file and fails when the Apache-2.0 header is missing.
            // Prepend it in the appropriate comment style for each file
            // so the checked-in artifacts satisfy the check.
            prepend_license_header(Path::new(&committed_bindings), "//");
            prepend_license_header(Path::new(&committed_wrapper), "//");
        }

        fn prepend_license_header(path: &Path, comment: &str) {
            let header = format!(
                "{c} Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/\n\
                 {c} SPDX-License-Identifier: Apache-2.0\n\
                 {c} @generated by libdd-profiling-heap-sampler/build.rs via bindgen; do not edit by hand.\n\
                 {c} Regenerate with: LIBDD_PROFILING_HEAP_SAMPLER_REGEN=1 cargo build -p libdd-profiling-heap-sampler\n\n",
                c = comment,
            );
            let body = std::fs::read_to_string(path).unwrap_or_else(|e| {
                panic!("reading {} to prepend license header: {e}", path.display())
            });
            std::fs::write(path, format!("{header}{body}"))
                .unwrap_or_else(|e| panic!("writing {} with license header: {e}", path.display()));
        }
    }
}
