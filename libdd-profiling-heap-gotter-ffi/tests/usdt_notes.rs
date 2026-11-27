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
//! The cdylib path is derived at runtime from the test executable location,
//! in whichever of cargo's two target layouts the test binary lives in: the
//! `deps` layout (stable toolchains and older nightlies), where the test
//! binary and the cdylib sit side by side in
//! `target/<[triple/]profile>/deps/`, and the `build` layout (newer
//! nightlies), where every unit's outputs live in their own
//! `target/<[triple/]profile>/build/<package>/<hash>/out/` directory.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};

/// Name of the crate's `cdylib` output.
const CDYLIB_NAME: &str = "liblibdd_profiling_heap_gotter_ffi.so";

fn cdylib_path() -> PathBuf {
    let exe = std::env::current_exe().expect("failed to read current executable path");
    let binary_dir = exe
        .parent()
        .expect("unexpected test executable path structure")
        .to_owned();

    // Directories that may hold the cdylib: the test binary's own directory
    // (the only location in the `deps` layout), plus, once the profile root
    // is known, the shared `deps` directory and every
    // `build/<package>/<hash>/out/` directory of the `build` layout.
    let mut dirs = vec![binary_dir.clone()];

    // Profile root: `<profile>/deps` or `<profile>/build/<package>/<hash>/out`.
    let profile_dir = if binary_dir.file_name().is_some_and(|name| name == "deps") {
        binary_dir.parent().map(Path::to_path_buf)
    } else {
        // out -> hash -> package -> build -> profile root
        let mut ancestors = binary_dir.ancestors().skip(1);
        let _hash = ancestors.next();
        let _package = ancestors.next();
        let build = ancestors.next();
        let profile = ancestors.next();
        match (build, profile) {
            (Some(build), Some(profile))
                if build.file_name().is_some_and(|name| name == "build") =>
            {
                Some(profile.to_path_buf())
            }
            _ => None,
        }
    };

    if let Some(profile) = profile_dir {
        let deps = profile.join("deps");
        if deps.is_dir() {
            dirs.push(deps);
        }
        if let Ok(packages) = std::fs::read_dir(profile.join("build")) {
            for package in packages.flatten() {
                let Ok(hashes) = std::fs::read_dir(package.path()) else {
                    continue;
                };
                for hash in hashes.flatten() {
                    let out = hash.path().join("out");
                    if out.is_dir() {
                        dirs.push(out);
                    }
                }
            }
        }
    }

    let mut candidates: Vec<PathBuf> = dirs
        .iter()
        .map(|dir| dir.join(CDYLIB_NAME))
        .filter(|path| path.is_file())
        .collect();
    assert!(
        !candidates.is_empty(),
        "no artifact `{CDYLIB_NAME}` found in {}",
        dirs.iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Several units may have built the cdylib (e.g. once per feature
    // combination); keep the most recently modified one.
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .expect("failed to read artifact metadata")
    });
    candidates.pop().expect("checked non-empty above")
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
