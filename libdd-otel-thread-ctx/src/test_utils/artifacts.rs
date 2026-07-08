// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers to locate a crate's build artifacts from within an integration test binary.
//!
//! A test binary and the crate artifacts it exercises (`cdylib`, `staticlib`) live side by side in
//! `target/<[triple/]profile>/deps/`, so the paths are derived from the running test executable.

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

/// Assert that `path` can be opened for reading.
pub fn check_readable(path: &Path) {
    assert!(
        std::fs::File::open(path).is_ok(),
        "{} could not be opened for reading",
        path.display()
    );
}
