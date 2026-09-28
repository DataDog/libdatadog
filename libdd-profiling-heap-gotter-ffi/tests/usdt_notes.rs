// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Verify the ddheap USDT probes appear exactly once in the built shared
//! library's `.note.stapsdt`. A duplicate entry (e.g. from `static inline` +
//! bindgen's `wrap_static_fns`, or LTO inlining across TUs) would make an
//! attached consumer fire twice, so this guards the "one note per probe"
//! invariant documented in the sampler's `probes.c`.
//!
//! Split into two tests because the two probes have different lifetimes:
//! `ddheap:alloc` is expected in every build, while `ddheap:free` only
//! exists when compiled with live-heap tracking (the `live-heap` feature) —
//! its absence otherwise is intentional, so it's covered by its own
//! feature-gated test rather than folded into a single "all probes present"
//! assertion.
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

/// `ddheap:alloc` is unconditional: every build samples allocations, so its
/// note must always be present exactly once.
#[test]
#[cfg_attr(miri, ignore)]
fn ddheap_alloc_probe_has_one_note() {
    let path = cdylib_path();
    libdd_profiling_heap_sampler::usdt_check::check_usdt_probes_in(&path, &["alloc"]).unwrap();
}

/// `ddheap:free` only exists when built with live-heap tracking. Gated on the
/// `live-heap` feature (forwarded to the sampler crate) so this test only
/// runs, and only asserts the note is present, in builds that opt in.
#[cfg(feature = "live-heap")]
#[test]
#[cfg_attr(miri, ignore)]
fn ddheap_free_probe_has_one_note() {
    let path = cdylib_path();
    libdd_profiling_heap_sampler::usdt_check::check_usdt_probes_in(&path, &["free"]).unwrap();
}
