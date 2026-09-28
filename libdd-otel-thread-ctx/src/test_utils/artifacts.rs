// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers to locate a crate's build artifacts from within an integration test binary.
//!
//! A test binary and the crate artifacts it exercises (`cdylib`, `staticlib`, `rlib`) live side by
//! side in `target/<[triple/]profile>/deps/`, so the paths are derived from the running test
//! executable.

use std::path::{Path, PathBuf};

/// Directory that holds the running test binary and the crate artifacts.
pub fn deps_dir() -> PathBuf {
    // test binary: target/<[triple/]profile>/deps/<name>
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .to_owned()
}

/// Path to the artifact named `name` sitting next to the running test binary.
pub fn artifact_path(name: &str) -> PathBuf {
    deps_dir().join(name)
}

/// Path to the `rlib` of the crate named `crate_name` (using underscores, as in the file name),
/// sitting next to the running test binary.
///
/// Unlike a `staticlib`/`cdylib`, an `rlib`'s file name is suffixed by rustc with a metadata hash
/// that isn't predictable ahead of time, so this globs `deps_dir()` for `lib<crate_name>-*.rlib`
/// instead of joining an exact name. If stale rlibs from a previous build linger in `deps_dir()`,
/// the most recently modified match is used.
pub fn rlib_path(crate_name: &str) -> PathBuf {
    let dir = deps_dir();
    let prefix = format!("lib{crate_name}-");

    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rlib"))
        })
        .collect();
    assert!(
        !candidates.is_empty(),
        "no rlib matching `{prefix}*.rlib` found in {}",
        dir.display()
    );

    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or_else(|e| panic!("failed to read metadata for {}: {e}", path.display()))
    });
    candidates.pop().expect("checked non-empty above")
}

/// Assert that `path` can be opened for reading.
pub fn check_readable(path: &Path) {
    assert!(
        std::fs::File::open(path).is_ok(),
        "{} could not be opened for reading",
        path.display()
    );
}
