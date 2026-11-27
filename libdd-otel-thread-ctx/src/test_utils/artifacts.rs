// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers to locate a crate's build artifacts from within an integration test binary.
//!
//! A test binary and the crate artifacts it exercises (`cdylib`, `staticlib`, `rlib`) live side by
//! side in `target/<[triple/]profile>/deps/`, so the paths are derived from the running test
//! executable. Newer nightly toolchains instead emit every unit's outputs, test binaries
//! included, into its own `target/<[triple/]profile>/build/<package>/<hash>/out/` directory;
//! the helpers below detect which of the two layouts the test executable lives in and search the
//! directories that hold the artifacts in that layout.

use std::path::{Path, PathBuf};

/// Directory that holds the running test binary.
///
/// In the classic layout this is `target/<[triple/]profile>/deps/`, where the crate artifacts
/// live too; see [`artifact_path`] and [`rlib_path`] for the artifact lookups that also
/// cover the `build/<package>/<hash>/out/` layout of newer nightlies.
pub fn deps_dir() -> PathBuf {
    // test binary: target/<[triple/]profile>/deps/<name>
    //         or: target/<[triple/]profile>/build/<package>/<hash>/out/<name>
    let exe = std::env::current_exe().expect("failed to read current executable path");
    exe.parent()
        .expect("unexpected test executable path structure")
        .to_owned()
}

/// All directories that may hold crate artifacts: the running test binary's directory, plus
/// every `build/<package>/<hash>/out/` directory when the binary lives in the newer nightly
/// layout where each unit's outputs sit in their own directory.
fn artifact_dirs() -> Vec<PathBuf> {
    let deps = deps_dir();
    let mut dirs = vec![deps.clone()];

    // Profile root of the running test binary:
    // `.../<profile>/deps` in the classic layout,
    // `.../<profile>/build/<package>/<hash>/out` in the newer nightly layout.
    let profile_dir = if deps.file_name().is_some_and(|name| name == "deps") {
        deps.parent().map(Path::to_path_buf)
    } else {
        let mut ancestors = deps.ancestors().skip(1); // hash, package, build, profile
        let _hash = ancestors.next();
        let _package = ancestors.next();
        match (ancestors.next(), ancestors.next()) {
            (Some(build), Some(profile))
                if build.file_name().is_some_and(|name| name == "build") =>
            {
                Some(profile.to_path_buf())
            }
            _ => None,
        }
    };

    if let Some(profile) = profile_dir
        && let Ok(packages) = std::fs::read_dir(profile.join("build"))
    {
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
    dirs
}

/// One-line description of the searched directories, for failure messages.
fn describe(dirs: &[PathBuf]) -> String {
    format!(
        "[{}]",
        dirs.iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Most recently modified of `paths`.
fn newest(paths: &mut Vec<PathBuf>) -> PathBuf {
    paths.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or_else(|e| panic!("failed to read metadata for {}: {e}", path.display()))
    });
    paths.pop().expect("checked non-empty by the caller")
}

/// Path to the artifact named `name` built for the profile the running test binary belongs to.
///
/// `cdylib`/`staticlib` outputs keep their exact name; if several copies exist (e.g. stale
/// copies from a previous build, or one per unit that built the artifact), the most recently
/// modified match is used.
pub fn artifact_path(name: &str) -> PathBuf {
    let dirs = artifact_dirs();
    let mut candidates: Vec<PathBuf> = dirs
        .iter()
        .map(|dir| dir.join(name))
        .filter(|path| path.is_file())
        .collect();

    assert!(
        !candidates.is_empty(),
        "no artifact named `{name}` found in {}",
        describe(&dirs)
    );

    newest(&mut candidates)
}

/// Path to the `rlib` of the crate named `crate_name` (using underscores, as in the file name),
/// built for the profile the running test binary belongs to.
///
/// Unlike a `staticlib`/`cdylib`, an `rlib`'s file name is suffixed by rustc with a metadata hash
/// that isn't predictable ahead of time, so this globs the artifact directories for
/// `lib<crate_name>-*.rlib` instead of joining an exact name. If stale rlibs from a previous
/// build linger, the most recently modified match is used.
pub fn rlib_path(crate_name: &str) -> PathBuf {
    let dirs = artifact_dirs();
    let prefix = format!("lib{crate_name}-");

    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            let matches = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rlib"));
            if matches {
                candidates.push(path);
            }
        }
    }

    assert!(
        !candidates.is_empty(),
        "no rlib matching `{prefix}*.rlib` found in {}",
        describe(&dirs)
    );

    newest(&mut candidates)
}

/// Assert that `path` can be opened for reading.
pub fn check_readable(path: &Path) {
    assert!(
        std::fs::File::open(path).is_ok(),
        "{} could not be opened for reading",
        path.display()
    );
}
