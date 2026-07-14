// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify the ddheap USDT probes appear exactly once in the built shared
//! library's `.note.stapsdt`. A duplicate entry (e.g. from `static inline` +
//! bindgen's `wrap_static_fns`, or LTO inlining across TUs) would make an
//! attached consumer fire twice, so this guards the "one note per probe"
//! invariant documented in the sampler's `probes.c`.
//!
//! Inspecting the linked cdylib (rather than running the check in-process)
//! validates the real shipped artifact. Mirrors
//! libdd-otel-thread-ctx-ffi/tests/elf_properties.rs.
//!
//! The cdylib path is derived at runtime from the test executable location.
//! Both the test binary and the cdylib live in `target/<[triple/]profile>/deps/`.

#![cfg(target_os = "linux")]

use std::path::PathBuf;

fn cdylib_path() -> PathBuf {
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .join("liblibdd_profiling_heap_gotter_ffi.so")
}

#[test]
#[cfg_attr(miri, ignore)]
fn ddheap_probes_have_one_note_each() {
    let path = cdylib_path();
    libdd_profiling_heap_sampler::usdt_check::check_usdt_probes_in(&path).unwrap();
}
