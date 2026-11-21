// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

fn main() {
    // Dependents (such as dd-trace-py's `_native`) find the C header through
    // DEP_DATADOG_PROFILING_FFI_INCLUDE; `links` carries nothing else.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo::metadata=include={manifest}/include");
    println!("cargo::rerun-if-changed=include/datadog/profiling_v2.h");
    // Which formats and compressors are built in, as
    // DEP_DATADOG_PROFILING_FFI_PPROF / _OTEL / _GZIP / _ZSTD. `otel` turns on
    // libdd-profiling's `gzip`, which this crate's CARGO_FEATURE_GZIP does not
    // show, so it counts as gzip here too.
    let set =
        |name: &str| std::env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some();
    for feature in ["pprof", "otel", "gzip", "zstd"] {
        let enabled = set(feature) || (feature == "gzip" && set("otel"));
        println!("cargo::metadata={feature}={}", u8::from(enabled));
    }
    // Set by scripts/check-profiling-ffi-no-panic.sh for the C artifact, and
    // for its deliberate-panic negative control.
    println!("cargo::rustc-check-cfg=cfg(ddprof_ffi_cdylib)");
    println!("cargo::rustc-check-cfg=cfg(ddprof_ffi_canary)");
    println!("cargo::rustc-check-cfg=cfg(ddprof_ffi_diagnose)");
}
